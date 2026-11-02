//! Editing node parameters: the control of one parameter, and the contents of the
//! window with a node's advanced parameters.
//!
//! A parameter is shown in one of two places, decided by its spec
//! ([`ParamSpec::advanced`](v_signal_nodes_core::ParamSpec::advanced)): everyday ones
//! on the node (`canvas::controls`), advanced ones in a window of their own
//! ([`windows`](super::windows)), opened with a button in the node's header. Both use
//! [`param_control`], which only reports the new value; every change goes through
//! `VSignalNodesApp::set_param`.
//!
//! | Parameter | Control                                                    |
//! |-----------|------------------------------------------------------------|
//! | number    | drag sideways (slower with `Shift`), or click and type     |
//! | choice    | a drop-down list                                           |
//! | toggle    | a button lit while on                                      |

use eframe::egui;
use v_signal_nodes_core::{ParamKind, ParamValue};

use super::{NodeWidget, caption};
use crate::editor::{Language, Text};

/// Part of a number's value one point of dragging changes.
const DRAG_RELATIVE_SPEED: f64 = 0.005;
/// Part of a number's range one point of dragging changes at least, if the range
/// reaches zero: there, a speed relative to the value alone would stop.
const DRAG_MIN_SPEED: f64 = 0.002;
/// Width of the controls in an advanced parameters window.
const WINDOW_CONTROL_WIDTH: f32 = 200.0;

/// Shows the control of parameter `index` of `node`, filling `ui`, in `language`.
/// Returns the new value if it was changed this frame.
pub(super) fn param_control(
    ui: &mut egui::Ui,
    node: &NodeWidget,
    index: usize,
    language: Language,
) -> Option<ParamValue> {
    match (&node.spec().params[index].kind, node.node.params()[index]) {
        (ParamKind::Number { min, max, .. }, ParamValue::Number(mut value)) => {
            let name = node.param_name(index, language);
            let speed = drag_speed(value, *min, *max);
            ui.add(
                egui::DragValue::new(&mut value)
                    .range(*min..=*max)
                    .speed(speed)
                    .prefix(format!("{name}: "))
                    .min_decimals(1)
                    .max_decimals(3),
            )
            .changed()
            .then_some(ParamValue::Number(value))
        }
        (ParamKind::Choice { options, .. }, ParamValue::Choice(current)) => {
            let mut selected = current;
            egui::ComboBox::from_id_salt(("node_param", node.id, index))
                .selected_text(node.param_label(index, language))
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for option in 0..options.len() {
                        let label = node.option_label(index, option, language);
                        ui.selectable_value(&mut selected, option, label);
                    }
                });
            (selected != current).then_some(ParamValue::Choice(selected))
        }
        (ParamKind::Toggle { .. }, ParamValue::Toggle(on)) => ui
            .add(egui::Button::selectable(
                on,
                node.param_label(index, language),
            ))
            .clicked()
            .then_some(ParamValue::Toggle(!on)),
        // `Node::set_param` keeps values of the kind the spec declares.
        _ => None,
    }
}

/// How much one point of dragging changes a number at `value` in `min..=max`:
/// relative to the value, so a range as wide as 20 Hz to 20 kHz stays usable at both
/// ends.
fn drag_speed(value: f32, min: f32, max: f32) -> f64 {
    let relative = f64::from(value.abs()) * DRAG_RELATIVE_SPEED;
    if min > 0.0 {
        relative
    } else {
        relative.max(f64::from(max - min) * DRAG_MIN_SPEED)
    }
}

/// The contents of `node`'s advanced parameters window, in `language`. Returns what
/// was changed this frame, as `(parameter index, new value)`.
pub(super) fn advanced_window(
    ui: &mut egui::Ui,
    node: &NodeWidget,
    language: Language,
) -> Vec<(usize, ParamValue)> {
    let layout = egui::Layout::centered_and_justified(egui::Direction::LeftToRight);
    caption(ui, language.text(Text::AdvancedParams));
    let size = egui::vec2(WINDOW_CONTROL_WIDTH, ui.spacing().interact_size.y);
    node.advanced_params()
        .filter_map(|index| {
            let edit = ui.allocate_ui_with_layout(size, layout, |ui| {
                param_control(ui, node, index, language)
            });
            edit.inner.map(|value| (index, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_follows_the_value_and_never_stops_at_zero() {
        // A wide range starting above zero: fine at the low end, quick at the high one.
        assert!(drag_speed(440.0, 20.0, 20_000.0) < 3.0);
        assert!(drag_speed(20_000.0, 20.0, 20_000.0) > 50.0);
        // A range reaching zero: from zero it still moves.
        assert!(drag_speed(0.0, 0.0, 2.0) > 0.0);
        assert!(drag_speed(0.0, -1.0, 1.0) > 0.0);
    }
}

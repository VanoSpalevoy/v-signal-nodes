//! Editing node parameters: the control of one parameter, and the windows with
//! nodes' advanced parameters.
//!
//! A parameter is shown in one of two places, decided by its spec
//! ([`ParamSpec::advanced`](v_signal_nodes_core::ParamSpec::advanced)): everyday ones
//! on the node (`canvas::controls`), advanced ones in a window of their own, opened
//! with the button in the node's header. Both use [`param_control`], which only
//! reports the new value; every change goes through `VSignalNodesApp::set_param`.
//!
//! | Parameter | Control                                                    |
//! |-----------|------------------------------------------------------------|
//! | number    | drag sideways (slower with `Shift`), or click and type     |
//! | choice    | a drop-down list                                           |
//! | toggle    | a button lit while on                                      |

use eframe::egui;
use v_signal_nodes_core::{ParamKind, ParamValue};

use super::{NodeWidget, caption};
use crate::editor::{Language, Text, VSignalNodesApp};

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

/// Shows the window of every node whose advanced parameters are open, and applies
/// what was changed in them.
pub(super) fn show_advanced_windows(app: &mut VSignalNodesApp, ui: &egui::Ui) {
    let mut edits = Vec::new();
    let mut closed = Vec::new();
    let layout = egui::Layout::centered_and_justified(egui::Direction::LeftToRight);
    // Only the first frame of a window uses it: it opens where its button was clicked.
    let pointer = ui.input(|input| input.pointer.latest_pos());
    for &node_id in &app.advanced_windows {
        let Some(node) = app.node(node_id) else {
            continue;
        };
        let mut open = true;
        let mut window = egui::Window::new(format!("{} #{}", node.title(app.language), node.id))
            .id(ui.scope_id().with(("advanced_params", node_id)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(WINDOW_CONTROL_WIDTH);
        if let Some(pointer) = pointer {
            window = window.default_pos(pointer);
        }
        window.show(ui.ctx(), |ui| {
            caption(ui, app.language.text(Text::AdvancedParams));
            let size = egui::vec2(WINDOW_CONTROL_WIDTH, ui.spacing().interact_size.y);
            for index in node.advanced_params() {
                let edit = ui.allocate_ui_with_layout(size, layout, |ui| {
                    param_control(ui, &node, index, app.language)
                });
                if let Some(value) = edit.inner {
                    edits.push((node_id, index, value));
                }
            }
        });
        if !open {
            closed.push(node_id);
        }
    }
    for (node_id, index, value) in edits {
        app.set_param(node_id, index, value);
    }
    for node_id in closed {
        app.toggle_advanced_window(node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::tests::add;
    use crate::editor::ui::headless::{self, Frames};
    use crate::editor::ui::{install_theme, render};
    use v_signal_nodes_core::NodeId;
    use v_signal_nodes_plugin_std::MIXER;

    /// The editor without a window: whole frames, with pointer events.
    struct Editor {
        frames: Frames,
        app: VSignalNodesApp,
    }

    impl Editor {
        /// An editor with a mixer, its advanced window opened with the pointer at `at`.
        fn with_window(at: egui::Pos2) -> (Self, NodeId) {
            let frames = Frames::new();
            install_theme(&frames.ctx);
            let mut app = VSignalNodesApp::default();
            let mixer = add(&mut app, MIXER, egui::pos2(500.0, 300.0));
            let mut editor = Self { frames, app };
            editor.frame(vec![egui::Event::PointerMoved(at)]);
            editor.app.toggle_advanced_window(mixer);
            editor.frame(Vec::new());
            editor.frame(Vec::new());
            (editor, mixer)
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let app = &mut self.app;
            self.frames.run(events, |ui| render(app, ui));
        }

        fn click(&mut self, pos: egui::Pos2) {
            for events in headless::click(pos) {
                self.frame(events);
            }
        }
    }

    #[test]
    fn the_advanced_window_opens_at_the_pointer_and_edits_the_node() {
        let at = egui::pos2(100.0, 300.0);
        let (mut editor, mixer) = Editor::with_window(at);
        assert_eq!(
            editor.app.graph.node(mixer).unwrap().param("mode"),
            Some(ParamValue::Choice(1))
        );

        // Mode, the first control, below the title and the caption.
        let mode = at + egui::vec2(60.0, 71.0);
        editor.click(mode);
        // Its first option, "mono", right below it.
        editor.click(mode + egui::vec2(0.0, 36.0));

        assert_eq!(
            editor.app.graph.node(mixer).unwrap().param("mode"),
            Some(ParamValue::Choice(0))
        );
    }

    #[test]
    fn a_window_goes_with_its_node() {
        let (mut editor, mixer) = Editor::with_window(egui::pos2(100.0, 300.0));

        editor.app.remove_node(mixer);
        editor.frame(Vec::new());

        assert!(editor.app.advanced_windows.is_empty());
    }

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

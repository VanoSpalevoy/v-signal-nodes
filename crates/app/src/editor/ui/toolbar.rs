//! Top toolbar.
//!
//! - **Snap to grid** — [`Command::SnapToGrid`]: snaps every node's top-left corner
//!   to a grid anchored at the canvas origin, moving nodes to the nearest free grid
//!   point where snapping would make them overlap (`VSignalNodesApp::snap_to_grid`).
//! - **Scale** — canvas zoom ([`SCALE_RANGE`], in [`SCALE_STEP`] steps) stored in
//!   `app.scale` and applied by the canvas; the View menu zooms in the same steps.
//! - **Sample rate** — the project's (`VSignalNodesApp::set_sample_rate`): common
//!   rates in a list ([`SAMPLE_RATE_PRESETS`]), any other in kHz in the field next to
//!   it, applied when the field is let go (each new rate restarts a real-time run).
//! - **Language** — switches the UI language; takes effect on the next frame.
//!
//! The toolbar wraps onto more lines when the window is narrow, which makes the
//! panel taller and the canvas shorter.

use eframe::egui;

use super::params::drag_speed;
use super::{caption, panel_frame};
use crate::document::SAMPLE_RATE_RANGE;
use crate::editor::commands::{Command, SCALE_RANGE, SCALE_STEP};
use crate::editor::{Language, Text, VSignalNodesApp};

/// Sample rates offered in the list, in Hz.
const SAMPLE_RATE_PRESETS: [u32; 5] = [44_100, 48_000, 96_000, 192_000, 384_000];

/// Shows the toolbar as a top panel and applies its actions to `app`.
pub(super) fn show(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    egui::Panel::top("toolbar")
        .resizable(false)
        .frame(panel_frame(super::PANEL_BG))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.set_min_height(34.0);
                ui.add_space(12.0);
                caption(ui, app.language.text(Text::Tools));
                ui.add_space(12.0);
                if ui
                    .add_enabled(
                        app.can_execute(Command::SnapToGrid),
                        egui::Button::new(app.language.text(Text::SnapToGrid)),
                    )
                    .clicked()
                {
                    app.execute(Command::SnapToGrid);
                }
                // Replace with icons.
                for command in [
                    Command::RunRealtime,
                    Command::StopRealtime,
                    Command::RunAsync,
                ] {
                    if ui.button(app.language.text(command.text())).clicked() {
                        app.execute(command);
                    }
                }
                ui.separator();
                ui.add(
                    egui::Slider::new(&mut app.scale, SCALE_RANGE)
                        .step_by(f64::from(SCALE_STEP))
                        .text(app.language.text(Text::Scale))
                        .show_value(true),
                );
                if ui.button(app.language.text(Text::ResetZoom)).clicked() {
                    app.execute(Command::ResetZoom);
                }
                ui.separator();
                caption(ui, app.language.text(Text::Settings));
                ui.add_space(4.0);
                sample_rate_control(app, ui);
                ui.add_space(8.0);
                ui.label(app.language.text(Text::Language));
                egui::ComboBox::from_id_salt("language")
                    .selected_text(app.language.native_name())
                    .show_ui(ui, |ui| {
                        for language in Language::ALL {
                            ui.selectable_value(
                                &mut app.language,
                                language,
                                language.native_name(),
                            );
                        }
                    });
            });
        });
}

/// The sample rate list and field. The field's value while it's being dragged or
/// typed waits in egui temp memory and becomes the rate only when let go.
fn sample_rate_control(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    let current = app.sample_rate();
    let unit = format!(
        " {}{}",
        app.language.text(Text::PrefixKilo),
        app.language.text(Text::UnitHertz)
    );
    let kilohertz = |rate: u32| f64::from(rate) / 1000.0;
    ui.label(app.language.text(Text::SampleRate));
    let mut chosen = None;
    egui::ComboBox::from_id_salt("sample_rate")
        .selected_text(format!("{}{unit}", kilohertz(current)))
        .show_ui(ui, |ui| {
            for rate in SAMPLE_RATE_PRESETS {
                let label = format!("{}{unit}", kilohertz(rate));
                if ui.selectable_label(rate == current, label).clicked() {
                    chosen = Some(rate);
                }
            }
        });

    let editing = ui.scope_id().with("sample_rate_field");
    let mut value = ui
        .data(|data| data.get_temp::<f64>(editing))
        .unwrap_or_else(|| kilohertz(current));
    let (min, max) = (
        kilohertz(*SAMPLE_RATE_RANGE.start()),
        kilohertz(*SAMPLE_RATE_RANGE.end()),
    );
    let speed = drag_speed(value as f32, min as f32, max as f32);
    let response = ui.add(
        egui::DragValue::new(&mut value)
            .range(min..=max)
            .speed(speed)
            .suffix(unit)
            .max_decimals(3),
    );
    if response.drag_stopped() || response.lost_focus() {
        chosen = Some((value * 1000.0).round() as u32);
    } else if response.changed() {
        ui.data_mut(|data| data.insert_temp(editing, value));
    }
    if let Some(rate) = chosen {
        ui.data_mut(|data| data.remove::<f64>(editing));
        app.set_sample_rate(rate);
    }
}

//! Top toolbar.
//!
//! - **Snap to grid** — snaps every node's top-left corner to a [`GRID_STEP`] grid
//!   anchored at the canvas origin, moving nodes to the nearest free grid point
//!   where snapping would make them overlap (`VSignalNodesApp::snap_to_grid`).
//! - **Scale** — canvas zoom ([`SCALE_RANGE`], in [`SCALE_STEP`] steps) stored in
//!   `app.scale` and applied by the canvas.
//! - **Language** — switches the UI language; takes effect on the next frame.
//!
//! The toolbar wraps onto more lines when the window is narrow, which makes the
//! panel taller and the canvas shorter.

use eframe::egui;

use super::{caption, panel_frame};
use crate::editor::{Language, Text, VSignalNodesApp};

/// Grid size used by Snap to grid, in canvas units.
const GRID_STEP: f32 = 50.0;
/// Allowed canvas zoom.
const SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// Zoom slider step. Coarse steps keep the number of distinct font sizes (each
/// one is rasterized into egui's font atlas) small.
const SCALE_STEP: f64 = 0.1;

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
                if ui.button(app.language.text(Text::SnapToGrid)).clicked() {
                    app.snap_to_grid(GRID_STEP);
                }
                ui.separator();
                ui.add(
                    egui::Slider::new(&mut app.scale, SCALE_RANGE)
                        .step_by(SCALE_STEP)
                        .text(app.language.text(Text::Scale))
                        .show_value(true),
                );
                ui.separator();
                caption(ui, app.language.text(Text::Settings));
                ui.add_space(4.0);
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

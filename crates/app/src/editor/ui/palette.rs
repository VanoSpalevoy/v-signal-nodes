//! Left node palette.
//!
//! One button per entry of `app.palette` (every [`NodeKind`] of the loaded
//! plugins, generators first), filled with the kind's color, with dark
//! ([`PANEL_BG`](super::PANEL_BG)) text and outline.
//!
//! - **Click** — requests a node of that kind via `app.requested_node`; the canvas
//!   adds it in the middle of the visible area.
//! - **Drag** — starts an egui drag-and-drop with the [`NodeKind`] as payload. The
//!   canvas draws the drop preview and adds the node where the button is released
//!   (see `canvas::finish_palette_drop`). Releasing anywhere else or pressing `Esc`
//!   cancels the drag; egui handles both and shows the grabbing cursor meanwhile.

use eframe::egui;

use super::{caption, node_color, panel_frame};
#[cfg(doc)]
use crate::editor::NodeKind;
use crate::editor::{Text, VSignalNodesApp};

/// Palette width, including the panel frame's margins.
const PALETTE_WIDTH: f32 = 136.0;
/// Height of one palette button.
const BUTTON_HEIGHT: f32 = 36.0;

/// Shows the palette as a fixed-width left panel.
pub(super) fn show(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    egui::Panel::left("palette")
        .resizable(false)
        .exact_size(PALETTE_WIDTH)
        .frame(panel_frame(super::PANEL_BG))
        .show(ui, |ui| {
            caption(ui, app.language.text(Text::Nodes));
            ui.add_space(4.0);
            for kind in &app.palette {
                let response = ui.add_sized(
                    [ui.available_width(), BUTTON_HEIGHT],
                    // Inverse of a node: kind-colored fill, dark text and outline.
                    egui::Button::new(
                        egui::RichText::new(kind.label(app.language))
                            .color(super::PANEL_BG)
                            .strong(),
                    )
                    .fill(node_color(kind.spec()))
                    .stroke(egui::Stroke::new(1.0, super::PANEL_BG))
                    .sense(egui::Sense::click_and_drag()),
                );

                response.dnd_set_drag_payload(kind.clone());
                if response.clicked() {
                    app.requested_node = Some(kind.clone());
                }
                // While a drag is in progress, egui shows the grabbing cursor itself.
                if response.hovered() && !egui::DragAndDrop::has_any_payload(ui.ctx()) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
            }
        });
}

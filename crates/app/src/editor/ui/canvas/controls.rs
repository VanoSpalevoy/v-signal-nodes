//! The parameter controls on the nodes: real egui widgets over the painted node,
//! scaled with the zoom like everything else on the canvas.
//!
//! They come after [`paint`](super::paint), so they are drawn above the nodes, and
//! after the node interaction areas, so they win clicks over the node's body. A node
//! with another one dragged over it has its controls left out: they'd show through
//! (see [`controls_hidden`]); painting gives it plain text
//! for that moment.

use std::sync::Arc;

use eframe::egui;

use super::{View, controls_hidden};
use crate::editor::VSignalNodesApp;
use crate::editor::ui::node::PARAM_HEIGHT;
use crate::editor::ui::params::param_control;

/// Font size of a control.
const FONT_SIZE: f32 = 12.0;
/// Horizontal padding inside a control.
const PADDING: f32 = 4.0;

/// Shows the controls of every visible node and applies what was changed.
pub(super) fn show(app: &mut VSignalNodesApp, ui: &mut egui::Ui, view: View, viewport: egui::Rect) {
    let language = app.language;
    let style = Arc::new(scaled_style(ui.style(), view));
    let mut edits = Vec::new();
    for node in app.nodes() {
        if !view.rect(node.rect()).intersects(viewport) || controls_hidden(app, &node) {
            continue;
        }
        for (row, index) in node.inline_params().enumerate() {
            // A child, not a scope: controls take no space in the canvas layout. Its
            // id comes from the node, so raising the node doesn't change it.
            let mut ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("canvas_node_param", node.id, index))
                    .max_rect(view.rect(node.param_rect(row)))
                    .style(Arc::clone(&style))
                    .layout(egui::Layout::centered_and_justified(
                        egui::Direction::LeftToRight,
                    )),
            );
            if let Some(value) = param_control(&mut ui, &node, index, language) {
                edits.push((node.id, index, value));
            }
        }
    }
    for (node_id, index, value) in edits {
        app.set_param(node_id, index, value);
    }
}

/// `base` with the widgets following the zoom and fitting a parameter row.
fn scaled_style(base: &egui::Style, view: View) -> egui::Style {
    let font = view.font(FONT_SIZE);
    let mut style = base.clone();
    for text_style in [egui::TextStyle::Body, egui::TextStyle::Button] {
        style.text_styles.insert(text_style, font.clone());
    }
    style.override_font_id = Some(font);
    style.spacing.interact_size.y = view.len(PARAM_HEIGHT);
    style.spacing.button_padding = egui::vec2(view.len(PADDING), 0.0);
    style.spacing.icon_width = view.len(FONT_SIZE);
    style.spacing.icon_width_inner = view.len(FONT_SIZE * 0.6);
    style.spacing.icon_spacing = view.len(PADDING);
    style
}

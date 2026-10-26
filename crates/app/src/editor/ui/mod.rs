//! Editor UI: panels, the node canvas and the editor-side node model.
//!
//! The UI is immediate mode: [`render`] runs every frame, draws everything from
//! [`VSignalNodesApp`] and applies that frame's input to it directly. No widget state
//! is kept outside the app (apart from small per-widget scratch in egui memory).
//!
//! # Layout
//!
//! ```text
//! ┌──────────────────────── menu bar ────────────────────────┐
//! ├──────────────────────── toolbar ─────────────────────────┤
//! ├─────────┬────────────────────────────────────────────────┤
//! │ palette │ canvas (scrolls both ways)                    ▐│
//! │         │                                               ▐│
//! │         │▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄│
//! ├─────────┴────────────────────────────────────────────────┤
//! │ debug                                                    │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! | Module      | Panel           | Role                                               |
//! |-------------|-----------------|----------------------------------------------------|
//! | [`menu`]    | top             | menu bar and keyboard shortcuts (commands)         |
//! | [`toolbar`] | top, below menu | grid snapping, zoom, language                      |
//! | [`debug`]   | bottom          | status line and the list of connections            |
//! | [`palette`] | left            | node kinds to drag onto the canvas                 |
//! | [`canvas`]  | central         | nodes and connections: drag, connect, delete       |
//! | [`node`]    | —               | node model and geometry ([`NodeWidget`], sockets)  |
//! | [`placement`] | —             | keeping nodes from overlapping                     |
//! | [`routing`] | —               | routing connections around nodes                   |
//!
//! # Conventions
//!
//! - Panel order in [`render`] matters: egui panels added earlier claim their space
//!   first, which is why the debug panel spans the full width under the palette.
//! - Colors are the constants below, nothing else hardcodes UI colors (node kinds
//!   use [`node_color`]). Primary text is [`TEXT`], secondary text [`TEXT_DIM`]
//!   (also egui's "weak" text); [`ACCENT`] marks anything interactive or selected.
//! - Panels share [`panel_frame`]: no outlines, only egui's thin separator lines.
//!   The theme is installed once at startup by [`install_theme`], not per frame.
//! - All visible editor text goes through
//!   [`Language::text`](super::localization::Language::text); node text comes
//!   from the plugin specs and is translated by them (see [`node`]).

mod canvas;
mod debug;
mod menu;
mod node;
mod palette;
mod placement;
mod routing;
mod toolbar;

use eframe::egui;
use v_signal_nodes_core::NodeSpec;

use super::VSignalNodesApp;
pub(super) use canvas::RouteCache;
#[cfg(test)]
pub(super) use node::std_kind;
pub(super) use node::{NodeKind, NodeWidget, SocketRef, node_size};
pub(super) use placement::{place_new_node, settle, snap};

/// Darkest background: text edits, slider rails and other "extreme" backgrounds.
const WINDOW_BG: egui::Color32 = egui::Color32::from_rgb(6, 7, 9);
/// Background of the node canvas; darker than the panels so nodes stand out.
const CANVAS_BG: egui::Color32 = egui::Color32::from_rgb(9, 10, 13);
/// Background of the side panels (toolbar, palette, debug) and of node bodies;
/// also the text and outline color of palette buttons.
const PANEL_BG: egui::Color32 = egui::Color32::from_rgb(15, 16, 20);
/// Resting fill of buttons, sliders and other controls.
const CONTROL_BG: egui::Color32 = egui::Color32::from_rgb(25, 27, 33);
/// Fill of a hovered control.
const CONTROL_HOVER: egui::Color32 = egui::Color32::from_rgb(35, 38, 46);
/// Fill of a pressed or open control.
const CONTROL_ACTIVE: egui::Color32 = egui::Color32::from_rgb(45, 49, 60);
/// Thin separators between panels and around popups.
const BORDER: egui::Color32 = egui::Color32::from_rgb(29, 31, 38);
/// Primary text.
const TEXT: egui::Color32 = egui::Color32::from_rgb(224, 227, 234);
/// Secondary text: captions, socket labels, parameters, debug output.
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(124, 130, 144);
/// Interactive highlight: connections, selection, hovered sockets, slider fill.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(108, 168, 255);
/// Errors and destructive actions.
const DANGER: egui::Color32 = egui::Color32::from_rgb(235, 96, 96);
/// Rounding of buttons and other controls.
const CONTROL_RADIUS: u8 = 6;

/// Draws the whole editor for one frame: keyboard shortcuts first (before any
/// widget sees the keys), then the panels in layout order.
pub(super) fn render(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    menu::handle_shortcuts(app, ui.ctx());
    menu::show(app, ui);
    toolbar::show(app, ui);
    debug::show(app, ui);
    palette::show(app, ui);
    canvas::show(app, ui);
}

/// Frame shared by the side panels: `fill` background and padding, no outline.
/// Panels are separated by egui's thin separator lines (colored [`BORDER`]).
pub(super) fn panel_frame(fill: egui::Color32) -> egui::Frame {
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(10, 8))
}

/// A small, dim, uppercase section caption ("NODES", "TOOLS", ...).
pub(super) fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(11.0)
            .strong()
            .color(TEXT_DIM),
    );
}

/// Installs the editor theme once at startup. The app is always dark, regardless
/// of the OS theme, so the theme is pinned to dark as well.
pub(super) fn install_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.visuals = visuals();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
    });
}

/// The editor's dark theme: egui's dark visuals recolored with the constants
/// above, with rounded controls, flat (borderless) widgets and no shadows.
fn visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = PANEL_BG;
    visuals.panel_fill = PANEL_BG;
    visuals.extreme_bg_color = WINDOW_BG;
    visuals.faint_bg_color = CANVAS_BG;
    visuals.code_bg_color = WINDOW_BG;
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(TEXT_DIM);
    visuals.hyperlink_color = ACCENT;
    visuals.error_fg_color = DANGER;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);

    let radius = egui::CornerRadius::same(CONTROL_RADIUS);
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    // Flat look: no shadows anywhere.
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;

    let widgets = &mut visuals.widgets;
    // Labels and separators.
    widgets.noninteractive.bg_fill = PANEL_BG;
    widgets.noninteractive.weak_bg_fill = PANEL_BG;
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = radius;
    // Interactive widgets: flat fills, no outline, brighter text when hovered/pressed.
    for (state, fill, text) in [
        (&mut widgets.inactive, CONTROL_BG, TEXT),
        (&mut widgets.hovered, CONTROL_HOVER, egui::Color32::WHITE),
        (&mut widgets.active, CONTROL_ACTIVE, egui::Color32::WHITE),
        (&mut widgets.open, CONTROL_ACTIVE, TEXT),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = egui::Stroke::NONE;
        state.fg_stroke = egui::Stroke::new(1.0, text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    visuals
}

/// Accent color of a node kind: the palette button fill, the node outline, header
/// tint and socket dots, and the drop preview, so a kind is recognizable everywhere.
///
/// The spec's own [`NodeSpec::color`] if it sets one, else decided by the node's
/// role (`core::NodeKind`): generators green, processors violet, outputs orange.
fn node_color(spec: &NodeSpec) -> egui::Color32 {
    use v_signal_nodes_core::NodeKind as Role;
    if let Some(color) = spec.color {
        return egui::Color32::from_rgb(color.r, color.g, color.b);
    }
    match spec.kind {
        Role::Generator => egui::Color32::from_rgb(82, 196, 126),
        Role::Processor => egui::Color32::from_rgb(152, 124, 250),
        Role::Output => egui::Color32::from_rgb(236, 128, 92),
    }
}

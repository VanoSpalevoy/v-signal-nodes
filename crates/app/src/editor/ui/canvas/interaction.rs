//! Canvas input: palette clicks and drops, and everything done to nodes with the
//! pointer (see the interaction table in the [`canvas`](super) docs).
//!
//! Every change goes through a `VSignalNodesApp` method; this module only turns
//! egui responses into those calls.

use eframe::egui;

use super::{Hovered, View, centered_on, delete_button_rect, socket_rect};
use crate::editor::{NodeKind, SocketRef, Text, VSignalNodesApp};
use v_signal_nodes_core::NodeId;

/// Distance from the viewport edge at which dragging a node starts scrolling (screen points).
const EDGE_SCROLL_ZONE: f32 = 32.0;
/// Maximum edge-scroll speed, in screen points per second.
const EDGE_SCROLL_SPEED: f32 = 600.0;

/// Adds the node requested by a palette click in the middle of the visible area
/// (or the nearest free spot, see `add_node_at`).
pub(super) fn add_requested_node(app: &mut VSignalNodesApp, view: View, viewport: egui::Rect) {
    let Some(kind) = app.requested_node.take() else {
        return;
    };
    let pos = centered_on(&kind, view.to_canvas(viewport.center()));
    app.add_node_at(kind, pos);
}

/// Adds the node centered on the pointer when a palette drag is released over
/// the canvas viewport (`pointer` is `None` otherwise). Releases elsewhere are
/// cleaned up by egui.
pub(super) fn finish_palette_drop(
    app: &mut VSignalNodesApp,
    ui: &egui::Ui,
    view: View,
    pointer: Option<egui::Pos2>,
) {
    if let Some(pointer) = pointer
        && ui.input(|input| input.pointer.any_released())
        && let Some(kind) = egui::DragAndDrop::take_payload::<NodeKind>(ui.ctx())
    {
        let pos = centered_on(&kind, view.to_canvas(pointer));
        app.add_node_at((*kind).clone(), pos);
    }
}

/// While a node is dragged with the pointer near the viewport edge, scrolls the view that way.
///
/// Speed grows linearly from 0 at [`EDGE_SCROLL_ZONE`] inside the edge to
/// [`EDGE_SCROLL_SPEED`] at the edge and beyond, and is scaled by frame time so it
/// doesn't depend on the frame rate. Requests repaints while scrolling, so the view
/// keeps moving with the pointer held still. The dragged node follows on its own,
/// since [`drag_node`] re-anchors it to the pointer every frame.
fn edge_scroll(ui: &egui::Ui, viewport: egui::Rect, pointer: egui::Pos2) {
    let axis_speed = |pos: f32, min: f32, max: f32| {
        if pos > max - EDGE_SCROLL_ZONE {
            ((pos - (max - EDGE_SCROLL_ZONE)) / EDGE_SCROLL_ZONE).min(1.0)
        } else if pos < min + EDGE_SCROLL_ZONE {
            -((min + EDGE_SCROLL_ZONE - pos) / EDGE_SCROLL_ZONE).min(1.0)
        } else {
            0.0
        }
    };
    let dt = ui.input(|input| input.stable_dt).min(0.1);
    let delta = egui::vec2(
        axis_speed(pointer.x, viewport.left(), viewport.right()),
        axis_speed(pointer.y, viewport.top(), viewport.bottom()),
    ) * EDGE_SCROLL_SPEED
        * dt;

    if delta != egui::Vec2::ZERO {
        // A negative scroll delta moves the content up/left, i.e. the view down/right.
        // The scroll area clamps the offset to the content bounds itself.
        ui.scroll_with_delta_animation(-delta, egui::style::ScrollAnimation::none());
        ui.ctx().request_repaint();
    }
}

/// Moves a dragged node so the point that was grabbed stays exactly under the pointer.
///
/// The position is absolute rather than accumulated from drag deltas, so the node
/// can't drift away from the cursor when the view scrolls mid-drag (edge scroll,
/// mouse wheel or trackpad).
///
/// The move itself is `VSignalNodesApp::drag_to` (the node may pass over others);
/// the whole gesture is one drag from `begin_drag` to `end_drag`, which resolves
/// the overlaps. After `cancel_drag` (`Esc`) the rest of the
/// gesture is ignored. The grab offset is stored in egui memory under the body's id.
fn drag_node(
    app: &mut VSignalNodesApp,
    ui: &egui::Ui,
    node_id: NodeId,
    response: &egui::Response,
    view: View,
    viewport: egui::Rect,
) {
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    if response.drag_started()
        && let Some(node) = app.node(node_id)
    {
        // Measure the grab offset from where the button went down, before the drag threshold moved the pointer.
        let press = ui
            .input(|input| input.pointer.press_origin())
            .unwrap_or(pointer);
        let grab = view.to_canvas(press) - node.pos;
        ui.data_mut(|data| data.insert_temp(response.id, grab));
        app.begin_drag(node_id);
    }
    if response.dragged() && app.dragged_node() == Some(node_id) {
        let grab: egui::Vec2 = ui
            .data(|data| data.get_temp(response.id))
            .unwrap_or_default();
        app.drag_to(node_id, view.to_canvas(pointer) - grab);
        edge_scroll(ui, viewport, pointer);
    }
    if response.drag_stopped() && app.dragged_node() == Some(node_id) {
        app.end_drag();
    }
}

/// Registers the interactive areas of every node and applies this frame's clicks
/// and drags: moving, raising, selecting a source, connecting, disconnecting and
/// deleting. Returns what the pointer is over, for hover feedback.
///
/// Deleting and raising change the node list, so they are applied after the loop
/// (at most one of each per frame).
pub(super) fn handle_node_interactions(
    app: &mut VSignalNodesApp,
    ui: &mut egui::Ui,
    view: View,
    viewport: egui::Rect,
) -> Hovered {
    let mut hovered = Hovered::default();
    let mut deleted_node_id = None;
    let mut raised_node_id = None;

    for node_id in app.z_order.clone() {
        let Some(node) = app.node(node_id) else {
            continue;
        };
        // Ids are derived from the node ID, not from creation order, so they stay
        // stable when nodes are added, deleted or reordered.
        let body = ui.interact(
            view.rect(node.rect()),
            ui.scope_id().with(("canvas_node", node_id)),
            egui::Sense::click_and_drag(),
        );
        let sockets: Vec<(SocketRef, egui::Response)> = node
            .sockets()
            .map(|socket| {
                let response = ui
                    .interact(
                        socket_rect(&node, socket, view),
                        ui.scope_id().with(("canvas_socket", socket)),
                        egui::Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                (socket, response)
            })
            .collect();
        let delete = ui
            .interact(
                view.rect(delete_button_rect(&node)),
                ui.scope_id().with(("canvas_node_delete", node_id)),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(app.language.text(Text::DeleteNode));

        drag_node(app, ui, node_id, &body, view, viewport);
        if body.clicked() || body.drag_started() {
            raised_node_id = Some(node_id);
        }
        if delete.hovered() {
            hovered.delete_button = Some(node_id);
        }
        if delete.clicked_by(egui::PointerButton::Primary) {
            deleted_node_id = Some(node_id);
            continue;
        }

        for (socket, response) in sockets {
            if response.hovered() {
                hovered.socket = Some(socket);
            }
            let primary = response.clicked_by(egui::PointerButton::Primary);
            let secondary = response.clicked_by(egui::PointerButton::Secondary);
            match socket {
                SocketRef::Output(output) if primary => app.select_source(output),
                SocketRef::Input(input) if primary => app.connect_pending(input),
                SocketRef::Output(output) if secondary => app.disconnect_output(output),
                SocketRef::Input(input) if secondary => app.disconnect_input(input),
                _ => {}
            }
        }
    }

    if let Some(node_id) = raised_node_id {
        app.raise_node(node_id);
    }
    if let Some(node_id) = deleted_node_id {
        app.remove_node(node_id);
    }
    hovered
}

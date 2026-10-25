//! Canvas input: palette clicks and drops, and everything done to nodes with the
//! pointer (see the interaction table in the [`canvas`](super) docs).
//!
//! Every change goes through a `VSignalNodesApp` method; this module only turns
//! egui responses into those calls.

use eframe::egui;

use super::{Hovered, View, delete_button_rect, drop_rect, socket_rect};
use crate::editor::{NodeConfig, NodeKind, SocketKind, SocketRef, Text, VSignalNodesApp};

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
    let size = NodeConfig::for_kind(&kind).size();
    app.add_node_at(kind, view.to_canvas(viewport.center()) - size * 0.5);
}

/// Adds the node when a palette drag is released over the canvas viewport
/// (`pointer` is `None` otherwise). Releases elsewhere are cleaned up by egui.
pub(super) fn finish_palette_drop(
    app: &mut VSignalNodesApp,
    ui: &egui::Ui,
    view: View,
    pointer: Option<egui::Pos2>,
) {
    if let Some(pos) = pointer
        && ui.input(|input| input.pointer.any_released())
        && let Some(kind) = egui::DragAndDrop::take_payload::<NodeKind>(ui.ctx())
    {
        let pos = drop_rect(&kind, pos, view, &app.nodes).min;
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
    node_id: usize,
    response: &egui::Response,
    view: View,
    viewport: egui::Rect,
) {
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    if response.drag_started()
        && let Some(node) = app.nodes.iter().find(|node| node.id == node_id)
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

    for node_index in 0..app.nodes.len() {
        let node = &app.nodes[node_index];
        // Ids are derived from the node ID, not from creation order, so they stay
        // stable when nodes are added, deleted or reordered.
        let body_response = ui.interact(
            view.rect(node.rect()),
            ui.scope_id().with(("canvas_node", node.id)),
            egui::Sense::click_and_drag(),
        );

        let socket_responses = |side: SocketKind, count: usize| -> Vec<egui::Response> {
            (0..count)
                .map(|index| {
                    ui.interact(
                        socket_rect(node, side, index, view),
                        ui.scope_id().with(("canvas_socket", node.id, side, index)),
                        egui::Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                })
                .collect()
        };
        let input_responses = socket_responses(SocketKind::Input, node.config.inputs.len());
        let output_responses = socket_responses(SocketKind::Output, node.config.outputs.len());
        let delete_response = ui
            .interact(
                view.rect(delete_button_rect(node)),
                ui.scope_id().with(("canvas_node_delete", node.id)),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(app.language.text(Text::DeleteNode));

        let node_id = node.id;
        drag_node(app, ui, node_id, &body_response, view, viewport);
        if body_response.clicked() || body_response.drag_started() {
            raised_node_id = Some(node_id);
        }

        for (side, responses) in [
            (SocketKind::Input, &input_responses),
            (SocketKind::Output, &output_responses),
        ] {
            if let Some(index) = responses.iter().position(egui::Response::hovered) {
                hovered.socket = Some(SocketRef {
                    node_id,
                    side,
                    index,
                });
            }
        }
        if delete_response.hovered() {
            hovered.delete_button = Some(node_id);
        }

        if delete_response.clicked_by(egui::PointerButton::Primary) {
            deleted_node_id = Some(node_id);
            continue;
        }

        if let Some(index) = output_responses
            .iter()
            .position(|response| response.clicked_by(egui::PointerButton::Primary))
        {
            app.pending_source = Some(SocketRef {
                node_id,
                side: SocketKind::Output,
                index,
            });
            app.connection_refused = false;
        }

        if let Some(index) = input_responses
            .iter()
            .position(|response| response.clicked_by(egui::PointerButton::Primary))
            && let Some(from) = app.pending_source.take()
        {
            app.connect(
                from,
                SocketRef {
                    node_id,
                    side: SocketKind::Input,
                    index,
                },
            );
        }

        if let Some(index) = output_responses
            .iter()
            .position(|response| response.clicked_by(egui::PointerButton::Secondary))
        {
            app.disconnect_output(SocketRef {
                node_id,
                side: SocketKind::Output,
                index,
            });
        }

        if let Some(index) = input_responses
            .iter()
            .position(|response| response.clicked_by(egui::PointerButton::Secondary))
        {
            app.disconnect_input(SocketRef {
                node_id,
                side: SocketKind::Input,
                index,
            });
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

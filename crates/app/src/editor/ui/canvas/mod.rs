//! Central node canvas: draws nodes and connections and handles all node interaction.
//!
//! | Module            | Role                                                        |
//! |-------------------|-------------------------------------------------------------|
//! | this one          | the scroll area, [`View`], frame order, shared geometry     |
//! | [`interaction`]   | pointer and palette input, turned into `VSignalNodesApp` calls |
//! | [`connections`]   | [`RouteCache`]: connection routes kept up to date per frame |
//! | [`paint`]         | drawing everything, read-only                               |
//!
//! # Coordinates
//!
//! Nodes store **canvas coordinates** (`NodeWidget::pos`): `(0, 0)` is the top-left
//! of the scrollable content, and one canvas unit is one point at zoom 1.0. egui
//! works in **screen coordinates**. Each frame builds a [`View`] that maps between
//! the two, using where the canvas `(0, 0)` currently is on screen (`origin`) and
//! the toolbar zoom (`app.scale`):
//!
//! ```text
//! screen = canvas * scale + origin        canvas = (screen - origin) / scale
//! ```
//!
//! All geometry constants below are in canvas units unless noted otherwise, and
//! are scaled through the view when drawn, so everything zooms together. Anything
//! stored in the app uses canvas coordinates.
//!
//! # Scrolling and size
//!
//! The canvas is a [`egui::ScrollArea::both`] with solid, always-visible scroll
//! bars at the bottom and right. Its content is never smaller than the viewport
//! and grows to fit the furthest node plus [`CANVAS_MARGIN`] (see [`content_size`]),
//! so moving a node right or down extends the canvas. Nodes can't go above or left
//! of `(0, 0)`: a node dragged against that edge stops while the pointer moves on,
//! and follows again from the same grab point once the pointer comes back.
//!
//! Changing the zoom keeps the canvas point in the middle of the view in place
//! (see [`show`]).
//!
//! # Interaction
//!
//! | Input                                   | Effect                                          |
//! |-----------------------------------------|-------------------------------------------------|
//! | drag node body                          | move the node over the others (auto-scrolls     |
//! |                                         | near the edges); on release it moves to a free  |
//! |                                         | spot nearby or pushes its neighbors aside       |
//! | `Esc` while dragging a node             | cancel the drag: the node goes back             |
//! | click or drag node body                 | bring the node to the front                     |
//! | primary click output socket             | select it as the pending source                 |
//! | primary click input socket              | connect the pending source to it, replacing     |
//! |                                         | the input's previous source                     |
//! | secondary click output socket           | remove every connection from that output        |
//! | secondary click input socket            | remove the connection into that input           |
//! | primary click `✕`                       | delete the node and its connections             |
//! | click empty canvas, or `Esc`            | clear the pending source                        |
//! | drop a palette button on the canvas     | add a node centered on the pointer              |
//! | click a palette button                  | add a node in the middle of the visible area    |
//!
//! Clicking an input always clears the selection. Every effect is a
//! `VSignalNodesApp` call: selecting goes through `select_source` / `clear_selection`,
//! connecting through `connect_pending` (→ `connect`, which refuses edges that
//! would create a cycle, including a node feeding itself, and flags that for the
//! debug panel), disconnecting through `disconnect_input` / `disconnect_output`,
//! moving through `begin_drag` / `drag_to` / `end_drag` / `cancel_drag`.
//!
//! Feedback: while a node is dragged, ghosts show where nodes will be once it is
//! dropped; the pending source gets an accent ring and a curve that follows the
//! pointer; hovered sockets and delete buttons light up and show a pointing-hand
//! cursor. Socket hit areas ([`SOCKET_HIT_SIZE`]) are larger than the drawn dot,
//! but stay within half of the gap between nodes, so neighbors never share one.
//!
//! Every interactive area uses an id derived from the node ID (not from creation
//! order), so ids stay stable when nodes are added, deleted or reordered.
//!
//! # Frame order
//!
//! 1. Allocate the content rect (also the click target for "empty canvas"),
//!    which fixes the [`View`] for this frame.
//! 2. Add palette requests (click or drop), then handle node input
//!    ([`interaction::handle_node_interactions`]). Widgets created later win
//!    overlapping clicks, so sockets and the delete button sit above their node
//!    body, and later nodes above earlier ones.
//! 3. Bring the connection routes up to date ([`RouteCache::update`], within
//!    [`connections::ROUTING_BUDGET`]; another frame is requested if some were
//!    deferred).
//! 4. Paint ([`paint::paint`]): grid, connections, nodes, the node drop preview,
//!    the pending-source curve, then the palette drop preview on top.

mod connections;
mod interaction;
mod paint;

use eframe::egui;

use super::placement::NODE_GAP;
use super::{NodeKind, node_size, place_new_node};
use crate::editor::{NodeWidget, SocketRef, VSignalNodesApp};
pub(in crate::editor) use connections::RouteCache;
use v_signal_nodes_core::{NodeId, SocketDirection};

/// Height of the tinted header band holding the title and the delete button.
const HEADER_HEIGHT: f32 = 28.0;
/// Side of the square delete button in the node header's right end.
const DELETE_BUTTON_SIZE: f32 = 18.0;
/// Distance of the delete button from the node's top-right corner.
const NODE_INSET: f32 = 5.0;
/// Side of a socket's square hit area; larger than the dot so it's easy to hit.
/// Outside the node it is cut to half of [`NODE_GAP`] (see [`socket_rect`]).
const SOCKET_HIT_SIZE: f32 = 20.0;
/// Shortest horizontal Bézier handle of a connection curve.
const CONNECTION_MIN_HANDLE: f32 = 20.0;
/// Free space kept to the right of and below the furthest node, so the canvas can grow.
const CANVAS_MARGIN: f32 = 200.0;

/// Maps canvas coordinates to screen coordinates for one frame.
#[derive(Clone, Copy)]
struct View {
    /// Screen position of the canvas origin `(0, 0)`.
    origin: egui::Vec2,
    /// Screen points per canvas unit.
    scale: f32,
}

impl View {
    fn pos(self, canvas: egui::Pos2) -> egui::Pos2 {
        (canvas.to_vec2() * self.scale + self.origin).to_pos2()
    }

    fn rect(self, canvas: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_max(self.pos(canvas.min), self.pos(canvas.max))
    }

    fn len(self, canvas: f32) -> f32 {
        canvas * self.scale
    }

    fn to_canvas(self, screen: egui::Pos2) -> egui::Pos2 {
        ((screen.to_vec2() - self.origin) / self.scale).to_pos2()
    }

    /// The proportional font of `size` canvas units.
    fn font(self, size: f32) -> egui::FontId {
        egui::FontId::proportional(self.len(size))
    }
}

/// What the pointer is over after interaction, for hover feedback when painting.
#[derive(Default)]
struct Hovered {
    socket: Option<SocketRef>,
    delete_button: Option<NodeId>,
}

/// The scroll state of the last frame, to keep the view centered across zoom changes.
#[derive(Clone, Copy)]
struct LastView {
    /// Zoom the frame was drawn at.
    scale: f32,
    /// Scroll offset, in screen points.
    offset: egui::Vec2,
    /// Size of the visible area, in screen points.
    viewport: egui::Vec2,
}

/// Shows the canvas in the central panel, inside a scroll area.
///
/// When the zoom changed since the last frame, the scroll offset is adjusted so
/// the canvas point in the middle of the view stays there, instead of the view
/// sliding towards or away from the canvas origin.
pub(super) fn show(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default()
        // Edge to edge: no padding or outline around the canvas.
        .frame(egui::Frame::NONE.fill(super::CANVAS_BG))
        .show(ui, |ui| {
            // Solid bars get their own strip at the bottom and right instead of
            // floating over the nodes and showing up only on hover.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            let last_view_id = ui.scope_id().with("canvas_last_view");
            let mut scroll_area = egui::ScrollArea::both()
                .id_salt("canvas")
                .auto_shrink(false)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
            if let Some(last) = ui.data(|data| data.get_temp::<LastView>(last_view_id))
                && last.scale != app.scale
            {
                let center = (last.offset + last.viewport * 0.5) / last.scale;
                let offset = center * app.scale - last.viewport * 0.5;
                // The scroll area clamps the far end to the (new) content size itself.
                scroll_area = scroll_area.scroll_offset(offset.max(egui::Vec2::ZERO));
            }
            let output = scroll_area.show(ui, |ui| show_content(app, ui));
            ui.data_mut(|data| {
                data.insert_temp(
                    last_view_id,
                    LastView {
                        scale: app.scale,
                        offset: output.state.clamped_offset(),
                        viewport: output.inner_rect.size(),
                    },
                );
            });
        });
}

/// One frame of the scrollable canvas content; see the module docs for the order of steps.
fn show_content(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    let viewport = ui.clip_rect();
    let (canvas_rect, background) = ui.allocate_exact_size(
        content_size(&app.node_rects(), app.scale, viewport.size()),
        egui::Sense::click(),
    );
    let view = View {
        origin: canvas_rect.min.to_vec2(),
        scale: app.scale,
    };
    let pointer = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|pos| viewport.contains(*pos));

    let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
    if background.clicked() || escape {
        app.clear_selection();
    }
    if escape {
        app.cancel_drag();
    }
    interaction::add_requested_node(app, view, viewport);
    interaction::finish_palette_drop(app, ui, view, pointer);
    let hovered = interaction::handle_node_interactions(app, ui, view, viewport);

    // Taken out while the nodes (borrowing the whole app) are looked at.
    let mut routes = std::mem::take(&mut app.routes);
    let nodes: Vec<_> = app.nodes().collect();
    let update = routes.update(&nodes, &app.graph, connections::ROUTING_BUDGET);
    app.routes = routes;
    if update.deferred > 0 {
        ui.ctx().request_repaint();
    }
    paint::paint(app, ui, view, viewport, &hovered, pointer);
}

/// Content fills at least the viewport and grows to fit every node (`rects`) plus
/// a margin.
fn content_size(rects: &[egui::Rect], scale: f32, viewport: egui::Vec2) -> egui::Vec2 {
    let extent = rects.iter().fold(egui::Vec2::ZERO, |extent, rect| {
        extent.max(rect.max.to_vec2() + egui::Vec2::splat(CANVAS_MARGIN))
    });
    (extent * scale).max(viewport)
}

/// Top-left corner (canvas coordinates) that centers a node of `kind` on `center`:
/// where palette clicks and drops ask `add_node_at` to put a node.
fn centered_on(kind: &NodeKind, center: egui::Pos2) -> egui::Pos2 {
    center - node_size(kind.spec()) * 0.5
}

/// Canvas rect of a node of `kind` dropped at screen `pointer`: centered on it, or
/// at the nearest free spot among the nodes at `rects`, exactly as `add_node_at`
/// will place it, so the preview shows the real landing spot.
fn drop_rect(kind: &NodeKind, pointer: egui::Pos2, view: View, rects: &[egui::Rect]) -> egui::Rect {
    let size = node_size(kind.spec());
    let pos = place_new_node(rects, size, centered_on(kind, view.to_canvas(pointer)));
    egui::Rect::from_min_size(pos, size)
}

/// A socket's hit area on screen: a [`SOCKET_HIT_SIZE`] square centered on the
/// socket, but reaching at most half of [`NODE_GAP`] outside the node, so the
/// hit areas of two nodes facing each other across the gap never overlap.
fn socket_rect(node: &NodeWidget, socket: SocketRef, view: View) -> egui::Rect {
    let side = socket.side();
    let center = node.socket_pos(side, socket.index());
    let half = SOCKET_HIT_SIZE * 0.5;
    let outside = half.min(NODE_GAP * 0.5);
    let (left, right) = match side {
        SocketDirection::Input => (outside, half),
        SocketDirection::Output => (half, outside),
    };
    view.rect(egui::Rect::from_min_max(
        egui::pos2(center.x - left, center.y - half),
        egui::pos2(center.x + right, center.y + half),
    ))
}

/// The delete button's area in canvas coordinates: at the right end of the
/// header, vertically centered in it.
fn delete_button_rect(node: &NodeWidget) -> egui::Rect {
    let rect = node.rect();
    egui::Rect::from_center_size(
        egui::pos2(
            rect.right() - NODE_INSET - DELETE_BUTTON_SIZE * 0.5,
            rect.top() + HEADER_HEIGHT * 0.5,
        ),
        egui::Vec2::splat(DELETE_BUTTON_SIZE),
    )
}

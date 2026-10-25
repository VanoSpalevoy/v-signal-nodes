//! Painting the canvas: grid, connections, nodes and the transient overlays.
//!
//! Nothing here changes the app; [`paint`] draws one frame from its state, in
//! back-to-front order.

use eframe::egui;
use std::collections::HashMap;

use super::{CONNECTION_MIN_HANDLE, HEADER_HEIGHT, Hovered, View, delete_button_rect, drop_rect};
use crate::editor::ui::routing::{self, Route};
use crate::editor::ui::{ACCENT, DANGER, NodeKind, PANEL_BG, TEXT, TEXT_DIM, node_color};
use crate::editor::{Language, NodeWidget, SocketKind, VSignalNodesApp};

/// Corner radius of a node.
const NODE_ROUNDING: f32 = 8.0;
/// Width of the kind-colored node outline; the node body itself has the panel color.
const NODE_OUTLINE_WIDTH: f32 = 1.5;
/// Opacity of the kind color tinting the node header.
const HEADER_TINT: f32 = 0.16;
/// Horizontal padding of the title from the node's left edge.
const TITLE_PADDING: f32 = 10.0;
/// Half the length of each stroke of the drawn delete cross.
const DELETE_CROSS_HALF: f32 = 3.5;
/// Radius of a socket dot.
const SOCKET_RADIUS: f32 = 5.0;
/// Extra radius of a hovered socket dot.
const SOCKET_HOVER_GROW: f32 = 1.5;
/// Gap between a socket dot's center and its label.
const SOCKET_LABEL_GAP: f32 = 11.0;
/// Width of a connection curve.
const CONNECTION_WIDTH: f32 = 2.0;
/// Spacing of the background dot grid; every other dot is a Snap-to-grid point.
const GRID_SPACING: f32 = 25.0;
/// The dot grid is skipped when its on-screen spacing would be denser than this.
const GRID_MIN_SCREEN_SPACING: f32 = 10.0;
/// Corner radius of routed (orthogonal) connections.
const ROUTE_CORNER_RADIUS: f32 = 10.0;
/// Points per rounded corner of a routed connection.
const ROUTE_CORNER_SEGMENTS: usize = 6;
/// Longest miter at a connection's bend, in half-widths; sharper bends get a
/// thinner join instead of a spike.
const MITER_LIMIT: f32 = 2.0;
/// Extra margin around a connection's bounds before it counts as off screen.
const CULL_PADDING: f32 = 4.0;
/// Background grid dots.
const GRID_DOT: egui::Color32 = egui::Color32::from_rgb(28, 30, 37);

/// Paints the canvas for one frame, back to front: the dot grid, connections
/// (fading from the source node's kind color to the target's), nodes, the
/// node drop preview, the pending-source curve, and the palette drop preview on
/// top. `viewport` is the visible screen area, `pointer` the pointer position if
/// it is over it.
pub(super) fn paint(
    app: &VSignalNodesApp,
    ui: &egui::Ui,
    view: View,
    viewport: egui::Rect,
    hovered: &Hovered,
    pointer: Option<egui::Pos2>,
) {
    let painter = ui.painter();
    paint_grid(painter, view, viewport);
    paint_connections(app, painter, view, viewport);
    paint_nodes(app, painter, view, hovered);
    paint_drop_preview(app, painter, view);
    paint_pending_connection(app, painter, view, ui.ctx().pointer_hover_pos());
    paint_palette_preview(app, ui.ctx(), painter, view, pointer);
}

/// Draws each connection along its route: the plain curve when nothing is in the
/// way, otherwise the orthogonal path around the nodes with rounded corners. The
/// line fades from the source node's kind color to the target node's.
///
/// Connections entirely outside `viewport` are skipped before any geometry is
/// built (by their route's bounds, padded for the line width and for a curve
/// bulging slightly past its sampled polyline).
fn paint_connections(
    app: &VSignalNodesApp,
    painter: &egui::Painter,
    view: View,
    viewport: egui::Rect,
) {
    let colors: HashMap<usize, egui::Color32> = app
        .nodes
        .iter()
        .map(|node| (node.id, node_color(node.kind())))
        .collect();
    let kind_color = |node_id: usize| colors.get(&node_id).copied().unwrap_or(ACCENT);
    let width = view.len(CONNECTION_WIDTH);
    for connection in app.routes.routes() {
        let bounds = connection.bounds.expand(CONNECTION_WIDTH + CULL_PADDING);
        if !view.rect(bounds).intersects(viewport) {
            continue;
        }
        let points = match &connection.route {
            Route::Curve(points) => egui::epaint::CubicBezierShape::from_points_stroke(
                points.map(|p| view.pos(p)),
                false,
                egui::Color32::TRANSPARENT,
                egui::Stroke::NONE,
            )
            .flatten(None),
            Route::Path(points) => {
                let points: Vec<_> = points.iter().map(|p| view.pos(*p)).collect();
                rounded_corners(&points, view.len(ROUTE_CORNER_RADIUS))
            }
        };
        paint_gradient_line(
            painter,
            &points,
            width,
            kind_color(connection.from.node_id),
            kind_color(connection.to.node_id),
        );
    }
}

/// Draws a polyline (screen coordinates) whose color changes from `start` to `end`
/// along its length, as one mesh (see [`gradient_line_mesh`]).
fn paint_gradient_line(
    painter: &egui::Painter,
    points: &[egui::Pos2],
    width: f32,
    start: egui::Color32,
    end: egui::Color32,
) {
    // One physical pixel, like egui's own feathering.
    let feather = 1.0 / painter.pixels_per_point();
    let mesh = gradient_line_mesh(points, width, feather, start, end);
    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// Triangle mesh of the polyline `points`, `width` wide, its color changing from
/// `start` to `end` along the length (per vertex, so smoothly).
///
/// Built like egui's strokes: across each point there are four vertices, the
/// edges fading out to transparent over `feather` for anti-aliasing; a line
/// thinner than `feather` is drawn `feather` wide and fainter instead. Joins are
/// mitered, with the miter at most [`MITER_LIMIT`] times the half-width, so the
/// line keeps its width through bends without gaps or overlapping segments.
fn gradient_line_mesh(
    points: &[egui::Pos2],
    width: f32,
    feather: f32,
    start: egui::Color32,
    end: egui::Color32,
) -> egui::Mesh {
    // Repeated points (flattened curves, rounded corners) have no direction.
    let mut path: Vec<egui::Pos2> = Vec::with_capacity(points.len());
    for &point in points {
        if path.last().is_none_or(|last| last.distance(point) > 1e-3) {
            path.push(point);
        }
    }
    let mut mesh = egui::Mesh::default();
    if path.len() < 2 {
        return mesh;
    }
    let total: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    let (inner, alpha) = if width > feather {
        ((width - feather) * 0.5, 1.0)
    } else {
        (0.0, width / feather)
    };
    let outer = inner + feather;
    let normal = |a: egui::Pos2, b: egui::Pos2| (b - a).normalized().rot90();

    mesh.reserve_vertices(path.len() * 4);
    mesh.reserve_triangles((path.len() - 1) * 6);
    let mut walked = 0.0;
    for (index, &point) in path.iter().enumerate() {
        let before = index.checked_sub(1).map(|i| normal(path[i], point));
        let after = path.get(index + 1).map(|&next| normal(point, next));
        let offset = match (before, after) {
            (Some(a), Some(b)) => {
                let bisector = (a + b).normalized();
                if bisector == egui::Vec2::ZERO || !bisector.is_finite() {
                    // Turning straight back: no miter to speak of.
                    a
                } else {
                    // Scaled so the edges stay `inner`/`outer` away from both segments.
                    bisector / bisector.dot(a).max(1.0 / MITER_LIMIT)
                }
            }
            (Some(n), None) | (None, Some(n)) => n,
            (None, None) => unreachable!("the path has at least two points"),
        };
        if index > 0 {
            walked += path[index - 1].distance(point);
        }
        let color = start
            .lerp_to_gamma(end, walked / total)
            .gamma_multiply(alpha);

        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(point + offset * outer, egui::Color32::TRANSPARENT);
        mesh.colored_vertex(point + offset * inner, color);
        mesh.colored_vertex(point - offset * inner, color);
        mesh.colored_vertex(point - offset * outer, egui::Color32::TRANSPARENT);
        if index > 0 {
            // Three quads to the previous point: outer fade, core, outer fade.
            let previous = base - 4;
            for k in 0..3 {
                mesh.add_triangle(previous + k, previous + k + 1, base + k);
                mesh.add_triangle(previous + k + 1, base + k + 1, base + k);
            }
        }
    }
    mesh
}

/// Polyline through `points` with every corner replaced by a quadratic arc of
/// up to `radius` (less on short segments, so arcs never overlap).
fn rounded_corners(points: &[egui::Pos2], radius: f32) -> Vec<egui::Pos2> {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return Vec::new();
    };
    let mut result = vec![*first];
    for corner in points.windows(3) {
        let [before, at, after] = [corner[0], corner[1], corner[2]];
        let r = radius
            .min(before.distance(at) * 0.5)
            .min(at.distance(after) * 0.5);
        let entry = at + (before - at).normalized() * r;
        let exit = at + (after - at).normalized() * r;
        for k in 0..=ROUTE_CORNER_SEGMENTS {
            let t = k as f32 / ROUTE_CORNER_SEGMENTS as f32;
            let u = 1.0 - t;
            let point =
                entry.to_vec2() * (u * u) + at.to_vec2() * (2.0 * u * t) + exit.to_vec2() * (t * t);
            result.push(point.to_pos2());
        }
    }
    result.push(*last);
    result
}

/// Draws a translucent curve from the pending source socket to the pointer.
fn paint_pending_connection(
    app: &VSignalNodesApp,
    painter: &egui::Painter,
    view: View,
    pointer: Option<egui::Pos2>,
) {
    let (Some(source), Some(pointer)) = (app.pending_source, pointer) else {
        return;
    };
    let Some(node) = app.node(source.node_id) else {
        return;
    };
    let start = view.pos(node.socket_pos(source.side, source.index));
    let stroke = egui::Stroke::new(view.len(CONNECTION_WIDTH), ACCENT.gamma_multiply(0.5));
    let curve = routing::bezier(start, pointer, view.len(CONNECTION_MIN_HANDLE));
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        curve,
        false,
        egui::Color32::TRANSPARENT,
        stroke,
    ));
}

/// Draws a dot grid over the visible part of the canvas, aligned to canvas
/// coordinates so it scrolls and zooms with the nodes. Skipped when zoomed out so
/// far that the dots would turn into noise.
fn paint_grid(painter: &egui::Painter, view: View, viewport: egui::Rect) {
    if view.len(GRID_SPACING) < GRID_MIN_SCREEN_SPACING {
        return;
    }
    let visible =
        egui::Rect::from_min_max(view.to_canvas(viewport.min), view.to_canvas(viewport.max));
    let radius = view.len(1.0).max(0.8);
    let first = (visible.min.to_vec2() / GRID_SPACING).floor() * GRID_SPACING;
    let mut y = first.y;
    while y <= visible.max.y {
        let mut x = first.x;
        while x <= visible.max.x {
            painter.circle_filled(view.pos(egui::pos2(x, y)), radius, GRID_DOT);
            x += GRID_SPACING;
        }
        y += GRID_SPACING;
    }
}

/// Draws every node in order: body (panel color), header tinted with
/// the kind color, kind-colored outline, title (`Kind #id`), parameters, sockets
/// with their labels, and the delete cross, with hover and pending-source
/// highlights. All text is resolved in the app's language and all sizes follow
/// the zoom.
fn paint_nodes(app: &VSignalNodesApp, painter: &egui::Painter, view: View, hovered: &Hovered) {
    let language = app.language;
    for node in &app.nodes {
        let rect = view.rect(node.rect());
        let rounding = view.len(NODE_ROUNDING);
        painter.rect_filled(rect, rounding, PANEL_BG);
        paint_header(painter, view, node, language);
        painter.rect_stroke(
            rect,
            rounding,
            egui::Stroke::new(view.len(NODE_OUTLINE_WIDTH), node_color(node.kind())),
            egui::StrokeKind::Inside,
        );
        for (index, param) in node.config.params.iter().enumerate() {
            painter.text(
                view.pos(node.param_pos(index)),
                egui::Align2::CENTER_CENTER,
                node.config.param_label(param, language),
                view.font(12.0),
                TEXT_DIM,
            );
        }
        paint_sockets(app, node, painter, view, hovered);
        paint_delete_button(painter, view, node, hovered.delete_button == Some(node.id));
    }
}

/// Draws a node's header: the band tinted with the kind color (rounded at the
/// top like the node), a faint line under it, and the title (`Kind #id`).
fn paint_header(painter: &egui::Painter, view: View, node: &NodeWidget, language: Language) {
    let color = node_color(node.kind());
    let rect = view.rect(node.rect());
    let header =
        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), view.len(HEADER_HEIGHT)));
    let top_rounding = view.len(NODE_ROUNDING).round().clamp(0.0, 255.0) as u8;
    painter.rect_filled(
        header,
        egui::CornerRadius {
            nw: top_rounding,
            ne: top_rounding,
            sw: 0,
            se: 0,
        },
        color.gamma_multiply(HEADER_TINT),
    );
    painter.hline(
        header.x_range(),
        header.bottom(),
        egui::Stroke::new(view.len(1.0), color.gamma_multiply(0.3)),
    );

    let title_rect = painter.text(
        egui::pos2(rect.left() + view.len(TITLE_PADDING), header.center().y),
        egui::Align2::LEFT_CENTER,
        node.kind().label(language),
        view.font(13.0),
        TEXT,
    );
    painter.text(
        egui::pos2(title_rect.right() + view.len(5.0), header.center().y),
        egui::Align2::LEFT_CENTER,
        format!("#{}", node.id),
        view.font(11.0),
        TEXT_DIM,
    );
}

/// Draws the delete button as a cross made of two lines, so it is always exactly
/// centered and crisp at any zoom. (A `✕` glyph comes from egui's fallback emoji
/// font, whose metrics differ from the main font, so it renders off-center.)
/// Dim at rest; a red rounded square with a white cross when hovered.
fn paint_delete_button(painter: &egui::Painter, view: View, node: &NodeWidget, hovered: bool) {
    let rect = view.rect(delete_button_rect(node));
    let cross_color = if hovered {
        painter.rect_filled(rect, view.len(4.0), DANGER.gamma_multiply(0.85));
        egui::Color32::WHITE
    } else {
        TEXT_DIM
    };
    let stroke = egui::Stroke::new(view.len(1.5), cross_color);
    let half = view.len(DELETE_CROSS_HALF);
    let center = rect.center();
    painter.line_segment(
        [
            center + egui::vec2(-half, -half),
            center + egui::vec2(half, half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center + egui::vec2(-half, half),
            center + egui::vec2(half, -half),
        ],
        stroke,
    );
}

/// Draws a node's socket dots and labels. Dots have the kind color with a ring
/// of the body color, so they read as cut into the outline; a hovered socket
/// grows and turns accent-colored, and the pending source gets an accent ring.
fn paint_sockets(
    app: &VSignalNodesApp,
    node: &NodeWidget,
    painter: &egui::Painter,
    view: View,
    hovered: &Hovered,
) {
    let color = node_color(node.kind());
    for socket in node.sockets() {
        let pos = view.pos(node.socket_pos(socket.side, socket.index));
        let (radius, fill) = if hovered.socket == Some(socket) {
            (view.len(SOCKET_RADIUS + SOCKET_HOVER_GROW), ACCENT)
        } else {
            (view.len(SOCKET_RADIUS), color)
        };
        painter.circle(
            pos,
            radius,
            fill,
            egui::Stroke::new(view.len(2.0), PANEL_BG),
        );
        if app.pending_source == Some(socket) {
            painter.circle_stroke(
                pos,
                radius + view.len(4.0),
                egui::Stroke::new(view.len(1.5), ACCENT),
            );
        }

        let (align, direction) = match socket.side {
            SocketKind::Input => (egui::Align2::LEFT_CENTER, 1.0),
            SocketKind::Output => (egui::Align2::RIGHT_CENTER, -1.0),
        };
        painter.text(
            pos + egui::vec2(direction * view.len(SOCKET_LABEL_GAP), 0.0),
            align,
            node.config
                .socket_label(socket.side, socket.index, app.language),
            view.font(11.0),
            TEXT_DIM,
        );
    }
}

/// Draws a translucent outline of a node of `color` at `rect` (canvas coordinates),
/// for previews of where a node will be; returns the rect on screen.
fn paint_ghost(
    painter: &egui::Painter,
    view: View,
    rect: egui::Rect,
    color: egui::Color32,
) -> egui::Rect {
    let rect = view.rect(rect);
    let rounding = view.len(NODE_ROUNDING);
    painter.rect_filled(rect, rounding, color.gamma_multiply(0.08));
    painter.rect_stroke(
        rect,
        rounding,
        egui::Stroke::new(view.len(NODE_OUTLINE_WIDTH), color.gamma_multiply(0.8)),
        egui::StrokeKind::Inside,
    );
    rect
}

/// While a node is dragged, draws a ghost wherever a node would move if it were
/// dropped now (`VSignalNodesApp::drop_moves`): the dragged node's landing spot,
/// or the new places of the neighbors it pushes aside.
fn paint_drop_preview(app: &VSignalNodesApp, painter: &egui::Painter, view: View) {
    for (node_id, pos) in app.drop_moves() {
        if let Some(node) = app.node(node_id) {
            let rect = egui::Rect::from_min_size(pos, node.rect().size());
            paint_ghost(painter, view, rect, node_color(node.kind()));
        }
    }
}

/// Draws a translucent node of the dragged palette kind exactly where it would be dropped.
fn paint_palette_preview(
    app: &VSignalNodesApp,
    ctx: &egui::Context,
    painter: &egui::Painter,
    view: View,
    pointer: Option<egui::Pos2>,
) {
    let (Some(kind), Some(pos)) = (egui::DragAndDrop::payload::<NodeKind>(ctx), pointer) else {
        return;
    };
    let rect = drop_rect(&kind, pos, view, &app.nodes);
    let preview_rect = paint_ghost(painter, view, rect, node_color(&kind));
    painter.text(
        preview_rect.center(),
        egui::Align2::CENTER_CENTER,
        kind.label(app.language),
        view.font(14.0),
        TEXT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, pos2};

    const RED: Color32 = Color32::from_rgb(255, 0, 0);
    const BLUE: Color32 = Color32::from_rgb(0, 0, 255);

    #[test]
    fn gradient_line_runs_from_start_to_end_color() {
        let points = [pos2(0.0, 0.0), pos2(50.0, 0.0), pos2(100.0, 0.0)];

        let mesh = gradient_line_mesh(&points, 3.0, 1.0, RED, BLUE);

        assert_eq!(mesh.vertices.len(), 12);
        assert_eq!(mesh.indices.len(), 2 * 6 * 3);
        // Core vertices: the end colors at the ends, the halfway blend in between.
        assert_eq!(mesh.vertices[1].color, RED);
        assert_eq!(mesh.vertices[5].color, RED.lerp_to_gamma(BLUE, 0.5));
        assert_eq!(mesh.vertices[9].color, BLUE);
        // Outer vertices fade out for anti-aliasing.
        assert_eq!(mesh.vertices[0].color, Color32::TRANSPARENT);
        // Core is `width - feather` wide, fade `feather` on each side.
        let mut ys: Vec<f32> = mesh.vertices[..4].iter().map(|v| v.pos.y).collect();
        ys.sort_by(f32::total_cmp);
        assert_eq!(ys, [-2.0, -1.0, 1.0, 2.0]);
    }

    #[test]
    fn gradient_line_keeps_its_width_through_a_bend() {
        let points = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(10.0, 10.0)];

        let mesh = gradient_line_mesh(&points, 3.0, 1.0, RED, BLUE);

        // The corner's core vertices sit on the miter, `inner` away from both legs.
        let corner = &mesh.vertices[4..8];
        let inner = corner[1].pos - pos2(10.0, 0.0);
        assert!((inner.x.abs() - 1.0).abs() < 1e-4 && (inner.y.abs() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn gradient_line_skips_repeated_points_and_thin_lines_fade() {
        let points = [pos2(0.0, 0.0), pos2(0.0, 0.0), pos2(10.0, 0.0)];

        let mesh = gradient_line_mesh(&points, 0.5, 1.0, RED, RED);

        assert_eq!(mesh.vertices.len(), 8);
        assert!(mesh.vertices.iter().all(|v| v.pos.is_finite()));
        assert_eq!(mesh.vertices[1].color, RED.gamma_multiply(0.5));
        assert!(gradient_line_mesh(&points[..2], 2.0, 1.0, RED, BLUE).is_empty());
    }
}

//! Keeping nodes from overlapping.
//!
//! Nodes keep at least [`NODE_GAP`] between each other (only a node being dragged
//! may pass over the others until it is dropped). Two strategies:
//!
//! - [`settle`] — for dropping a dragged node: if a free spot is within
//!   [`DROP_SNAP_DISTANCE`], the node moves there; otherwise it stays where it
//!   was dropped and pushes its neighbors aside, which may push theirs in turn.
//! - [`free_pos`] — for adding ([`place_new_node`]) and snapping ([`snap`]): the
//!   nearest free position to the wanted one, searched outwards on a grid.
//!
//! Everything is in canvas coordinates, and positions never go negative.

use std::collections::VecDeque;

use eframe::egui;

/// Minimum free space between two nodes, in canvas units.
pub(super) const NODE_GAP: f32 = 10.0;
/// Grid step used when looking for a free spot for a new node.
const PLACEMENT_STEP: f32 = 10.0;
/// Rings searched by [`free_pos`] before giving up and returning the wanted position.
const MAX_SEARCH_RINGS: i32 = 200;
/// How far [`settle`] moves a dropped node to get it clear of its neighbors. A node
/// dropped deeper into others stays put and pushes them aside instead.
const DROP_SNAP_DISTANCE: f32 = 100.0;
/// Tolerance of every gap check, in canvas units. A node placed at exactly
/// [`NODE_GAP`] can end up a rounding error closer; without the tolerance it would
/// still count as too close to its neighbor and be moved again.
const EPSILON: f32 = 0.01;

/// The zone around `rect` another node must stay out of: [`NODE_GAP`] wide, minus
/// [`EPSILON`].
fn keep_out(rect: egui::Rect) -> egui::Rect {
    rect.expand(NODE_GAP - EPSILON)
}

/// Whether `a` and `b` overlap or are closer than [`NODE_GAP`]. Touching at
/// exactly the gap (within [`EPSILON`]) is allowed.
fn too_close(a: egui::Rect, b: egui::Rect) -> bool {
    let b = keep_out(b);
    a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y
}

/// Whether a node at `rect` would be too close to any of `obstacles`.
pub(super) fn blocked(rect: egui::Rect, obstacles: &[egui::Rect]) -> bool {
    obstacles.iter().any(|obstacle| too_close(rect, *obstacle))
}

/// New top-left positions of all `rects` (node rects, in the same order) after
/// the node at index `dropped` was dropped where it is now.
///
/// 1. If the dropped node is free, or a free spot is at most
///    [`DROP_SNAP_DISTANCE`] away ([`nearest_free_within`]), only the dropped node
///    moves there. Next to a lone neighbor that is usually the case: getting above
///    or below it takes at most the dropped node's height plus the gap.
/// 2. Otherwise it was dropped deep into a group of nodes on purpose: it stays and
///    pushes them aside ([`push_apart`]).
///
/// Whatever happens, the result has no two nodes closer than [`NODE_GAP`] (given
/// that the nodes other than `dropped` were apart before) and no negative position.
pub(in crate::editor) fn settle(rects: &[egui::Rect], dropped: usize) -> Vec<egui::Pos2> {
    let mut rects = rects.to_vec();
    let rect = rects[dropped];
    if let Some(pos) = nearest_free_within(rect, &all_but(&rects, dropped), DROP_SNAP_DISTANCE) {
        rects[dropped] = egui::Rect::from_min_size(pos, rect.size());
    } else {
        push_apart(&mut rects, dropped);
    }
    rects.iter().map(|rect| rect.min).collect()
}

/// The free top-left position for `rect` closest to where it is now, if one is at
/// most `max_distance` away (`rect`'s own position when it is already free).
///
/// Exact, not a grid search: the closest free spot either keeps an axis of `rect`
/// or touches the keep-out zone of an obstacle on it, so the candidates are every
/// mix of `rect`'s own x/y and the edges of the obstacles nearby.
fn nearest_free_within(
    rect: egui::Rect,
    obstacles: &[egui::Rect],
    max_distance: f32,
) -> Option<egui::Pos2> {
    // Only obstacles that reach into the search area can give a close enough edge.
    let area = rect.expand(max_distance + NODE_GAP);
    let near: Vec<_> = obstacles
        .iter()
        .filter(|obstacle| obstacle.intersects(area))
        .collect();
    let mut xs = vec![rect.min.x];
    let mut ys = vec![rect.min.y];
    for obstacle in &near {
        xs.extend([
            obstacle.min.x - NODE_GAP - rect.width(),
            obstacle.max.x + NODE_GAP,
        ]);
        ys.extend([
            obstacle.min.y - NODE_GAP - rect.height(),
            obstacle.max.y + NODE_GAP,
        ]);
    }
    let max_sq = max_distance * max_distance;
    xs.iter()
        .flat_map(|&x| ys.iter().map(move |&y| egui::pos2(x, y)))
        .filter(|pos| pos.x >= 0.0 && pos.y >= 0.0 && pos.distance_sq(rect.min) <= max_sq)
        .filter(|pos| !blocked(egui::Rect::from_min_size(*pos, rect.size()), obstacles))
        .min_by(|a, b| a.distance_sq(rect.min).total_cmp(&b.distance_sq(rect.min)))
}

/// Pushes every node out of the way of node `fixed`, which stays put.
///
/// Breadth-first from `fixed`: each node too close to an already placed one is
/// moved out of its way by the shortest push ([`push_out`]), counts as placed and
/// pushes the nodes it now touches in turn. Every node moves at most once, so this
/// always ends. A pushed node can land against one placed earlier; such leftovers
/// go to the nearest free spot ([`free_pos`]) at the end.
fn push_apart(rects: &mut [egui::Rect], fixed: usize) {
    let mut placed = vec![false; rects.len()];
    placed[fixed] = true;
    let mut queue = VecDeque::from([fixed]);
    while let Some(pusher) = queue.pop_front() {
        for index in 0..rects.len() {
            if placed[index] || !too_close(rects[index], rects[pusher]) {
                continue;
            }
            rects[index] = rects[index].translate(push_out(rects[index], rects[pusher]));
            placed[index] = true;
            queue.push_back(index);
        }
    }

    for index in (0..rects.len()).filter(|index| *index != fixed) {
        let others = all_but(rects, index);
        let rect = rects[index];
        if blocked(rect, &others) {
            let pos = free_pos(rect.min, rect.size(), &others, PLACEMENT_STEP);
            rects[index] = egui::Rect::from_min_size(pos, rect.size());
        }
    }
}

/// The shortest move (left, right, up or down) that takes `rect` out of the
/// keep-out zone of `pusher`, leaving exactly [`NODE_GAP`] between them. Moves that
/// would take `rect` to negative coordinates are skipped; right and down always work.
fn push_out(rect: egui::Rect, pusher: egui::Rect) -> egui::Vec2 {
    [
        egui::vec2(pusher.max.x + NODE_GAP - rect.min.x, 0.0),
        egui::vec2(pusher.min.x - NODE_GAP - rect.max.x, 0.0),
        egui::vec2(0.0, pusher.max.y + NODE_GAP - rect.min.y),
        egui::vec2(0.0, pusher.min.y - NODE_GAP - rect.max.y),
    ]
    .into_iter()
    .filter(|push| {
        let moved = rect.min + *push;
        moved.x >= 0.0 && moved.y >= 0.0
    })
    .min_by(|a, b| a.length_sq().total_cmp(&b.length_sq()))
    .unwrap_or_default()
}

/// The free top-left position closest to `wanted` for a node of `size`, on a
/// grid of `step` around `wanted` (so a grid-aligned `wanted` stays aligned).
///
/// Searches square rings of growing radius and takes the closest free candidate
/// of the first ring that has one. Falls back to `wanted` if nothing is free
/// within [`MAX_SEARCH_RINGS`] rings.
fn free_pos(
    wanted: egui::Pos2,
    size: egui::Vec2,
    obstacles: &[egui::Rect],
    step: f32,
) -> egui::Pos2 {
    let wanted = wanted.max(egui::Pos2::ZERO);
    let is_free = |pos: egui::Pos2| !blocked(egui::Rect::from_min_size(pos, size), obstacles);
    if is_free(wanted) {
        return wanted;
    }
    for ring in 1..=MAX_SEARCH_RINGS {
        let best = ring_offsets(ring)
            .map(|(dx, dy)| wanted + egui::vec2(dx as f32, dy as f32) * step)
            .filter(|pos| pos.x >= 0.0 && pos.y >= 0.0 && is_free(*pos))
            .min_by(|a, b| a.distance_sq(wanted).total_cmp(&b.distance_sq(wanted)));
        if let Some(pos) = best {
            return pos;
        }
    }
    wanted
}

/// New top-left positions of all `rects` snapped to the nearest point of a `step`
/// grid (anchored at the canvas origin) that keeps clear of the others. Nodes are
/// placed one by one, each against the current positions of all the others.
pub(in crate::editor) fn snap(rects: &[egui::Rect], step: f32) -> Vec<egui::Pos2> {
    let mut rects = rects.to_vec();
    for index in 0..rects.len() {
        let rect = rects[index];
        let snapped = (rect.min / step).round() * step;
        let pos = free_pos(snapped, rect.size(), &all_but(&rects, index), step);
        rects[index] = egui::Rect::from_min_size(pos, rect.size());
    }
    rects.iter().map(|rect| rect.min).collect()
}

/// Every rect of `rects` except the one at `index`.
fn all_but(rects: &[egui::Rect], index: usize) -> Vec<egui::Rect> {
    rects
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .map(|(_, rect)| *rect)
        .collect()
}

/// Where a new node of `size` wanted at `wanted` actually goes: the nearest spot
/// (on a [`PLACEMENT_STEP`] grid around `wanted`) that keeps clear of the nodes at
/// `rects`.
pub(in crate::editor) fn place_new_node(
    rects: &[egui::Rect],
    size: egui::Vec2,
    wanted: egui::Pos2,
) -> egui::Pos2 {
    free_pos(wanted, size, rects, PLACEMENT_STEP)
}

/// Grid offsets on the square ring at Chebyshev distance `ring` from the center.
fn ring_offsets(ring: i32) -> impl Iterator<Item = (i32, i32)> {
    (-ring..=ring).flat_map(move |dx| {
        (-ring..=ring)
            .filter(move |dy| dx.abs() == ring || dy.abs() == ring)
            .map(move |dy| (dx, dy))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(100.0, 50.0))
    }

    /// Asserts no two of `positions` (of [`rect`]-sized nodes) are too close or negative.
    fn assert_apart(positions: &[egui::Pos2]) {
        let rects: Vec<_> = positions.iter().map(|pos| rect(pos.x, pos.y)).collect();
        for (i, a) in rects.iter().enumerate() {
            assert!(a.min.x >= 0.0 && a.min.y >= 0.0, "{a:?} is negative");
            for b in &rects[i + 1..] {
                assert!(!too_close(*a, *b), "{a:?} and {b:?} are too close");
            }
        }
    }

    #[test]
    fn settle_keeps_a_free_drop() {
        let rects = [rect(0.0, 0.0), rect(500.0, 0.0)];

        assert_eq!(settle(&rects, 1), [rects[0].min, rects[1].min]);
    }

    #[test]
    fn settle_nudges_a_node_that_barely_overlaps() {
        // Dropped 5 units into the neighbor's gap on the right.
        let rects = [rect(0.0, 0.0), rect(105.0, 0.0)];

        let positions = settle(&rects, 1);

        // Only the dropped node moves, just enough to keep the gap.
        assert_eq!(
            positions,
            [egui::pos2(0.0, 0.0), egui::pos2(100.0 + NODE_GAP, 0.0)]
        );
    }

    #[test]
    fn settle_moves_the_dropped_node_up_to_the_snap_distance() {
        // Dropped half onto a lone neighbor: the closest free spot (60 up) is within
        // `DROP_SNAP_DISTANCE`, so only the dropped node moves.
        let rects = [rect(200.0, 100.0), rect(150.0, 100.0)];

        let positions = settle(&rects, 1);

        assert_eq!(
            positions,
            [egui::pos2(200.0, 100.0), egui::pos2(150.0, 40.0)]
        );
    }

    /// A column of `count` nodes at `x = 200`, from `y = 100` down, the gap apart.
    fn column(count: usize) -> Vec<egui::Rect> {
        (0..count)
            .map(|i| rect(200.0, 100.0 + i as f32 * (50.0 + NODE_GAP)))
            .collect()
    }

    #[test]
    fn settle_pushes_neighbors_away_from_a_deep_drop() {
        // Dropped right onto the middle of a column: every free spot is more than
        // `DROP_SNAP_DISTANCE` away, so the dropped node stays and the one under it
        // makes room (downwards, the shortest push), pushing the next one too.
        let mut rects = column(3);
        rects.push(rects[1]);

        let positions = settle(&rects, 3);

        assert_eq!(positions[3], rects[1].min, "the dropped node moved");
        assert_eq!(positions[0], rects[0].min, "the node above moved");
        assert_eq!(positions[1], rects[2].min);
        assert_eq!(
            positions[2],
            rects[2].min + egui::vec2(0.0, 50.0 + NODE_GAP)
        );
        assert_apart(&positions);
    }

    #[test]
    fn settle_pushes_in_a_chain() {
        // Dropped onto the second node of a column of five: the rest of the column
        // moves down one step each.
        let mut rects = column(5);
        rects.push(rects[1]);

        let positions = settle(&rects, 5);

        let step = egui::vec2(0.0, 50.0 + NODE_GAP);
        assert_eq!(positions[5], rects[1].min);
        assert_eq!(positions[0], rects[0].min);
        for i in 1..5 {
            assert_eq!(positions[i], rects[i].min + step, "node {i}");
        }
        assert_apart(&positions);
    }

    #[test]
    fn settle_never_pushes_into_negative_coordinates() {
        // Dropped into the canvas corner onto a node, with nodes to its right and
        // below: no free spot within reach, and pushing left or up is impossible.
        let rects = [
            rect(0.0, 0.0),
            rect(0.0, 50.0 + NODE_GAP),
            rect(100.0 + NODE_GAP, 0.0),
            rect(0.0, 0.0),
        ];

        let positions = settle(&rects, 3);

        assert_eq!(positions[3], egui::pos2(0.0, 0.0));
        assert_apart(&positions);
    }

    #[test]
    fn settle_resolves_a_crowded_drop() {
        // Dropped in the middle of a tight cluster.
        let mut rects: Vec<_> = (0..3)
            .flat_map(|x| (0..3).map(move |y| (x, y)))
            .map(|(x, y)| rect(x as f32 * (100.0 + NODE_GAP), y as f32 * (50.0 + NODE_GAP)))
            .collect();
        rects.push(rect(100.0, 50.0));

        let positions = settle(&rects, rects.len() - 1);

        assert_eq!(positions.last(), Some(&egui::pos2(100.0, 50.0)));
        assert_apart(&positions);
    }

    #[test]
    fn free_pos_finds_the_nearest_gap() {
        let obstacles = [rect(0.0, 0.0)];
        let size = egui::vec2(100.0, 50.0);

        let pos = free_pos(egui::pos2(0.0, 0.0), size, &obstacles, 10.0);

        assert!(!blocked(egui::Rect::from_min_size(pos, size), &obstacles));
        assert!(pos.x >= 0.0 && pos.y >= 0.0);
        // Nearest free spot is just below (50 + gap), closer than to the right (100 + gap).
        assert_eq!(pos, egui::pos2(0.0, 60.0));
    }

    #[test]
    fn free_pos_keeps_a_free_position() {
        let pos = free_pos(
            egui::pos2(500.0, 500.0),
            egui::vec2(100.0, 50.0),
            &[rect(0.0, 0.0)],
            10.0,
        );
        assert_eq!(pos, egui::pos2(500.0, 500.0));
    }
}

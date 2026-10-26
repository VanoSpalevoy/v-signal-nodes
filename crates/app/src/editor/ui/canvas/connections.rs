//! The routes of all connections, kept up to date across frames.
//!
//! [`RouteCache`] lives in the app (it is derived data, never saved) rather than in
//! egui memory, so it is plain Rust that unit tests can drive without a UI.

use eframe::egui;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use v_signal_nodes_core::{Graph, InputRef, NodeId, OutputRef, SocketDirection};

use super::CONNECTION_MIN_HANDLE;
use crate::editor::NodeWidget;
use crate::editor::ui::routing::{self, Route};

/// Time per frame for recomputing connection routes (see [`RouteCache::update`]);
/// the rest wait for the next frames.
pub(super) const ROUTING_BUDGET: Duration = Duration::from_millis(4);
/// Share of [`ROUTING_BUDGET`] that may go to refreshing routes that are outdated
/// but still fit to draw; the rest is kept for routes that must change.
const REFRESH_SHARE: f32 = 0.5;

/// One connection's route, with what later connections need to route around it.
pub(super) struct RoutedConnection {
    /// The input socket (each input has at most one connection).
    pub(super) to: InputRef,
    /// The output socket feeding `to`.
    pub(super) from: OutputRef,
    /// Hash of everything the route was computed from (see [`RouteCache::update`]).
    key: u64,
    /// The output and input socket positions the route runs between.
    ends: [egui::Pos2; 2],
    /// A stand-in (the plain curve) for a connection that wasn't routed for its
    /// current ends because the frame's budget ran out; replaced as soon as possible.
    provisional: bool,
    /// How to draw the connection, in canvas coordinates.
    pub(super) route: Route,
    /// The route as a polyline, a wire for the connections routed after it.
    polyline: Vec<egui::Pos2>,
    /// Hash of `polyline`: later connections' keys depend on this, not on `key`, so
    /// recomputing a route that comes out the same doesn't reroute them.
    shape: u64,
    /// Bounds of `polyline`, also used by the canvas to skip connections off screen.
    pub(super) bounds: egui::Rect,
}

impl RoutedConnection {
    /// The connection from `from` to `to`, routed as `route` for its current `ends`.
    fn routed(
        to: InputRef,
        from: OutputRef,
        ends: [egui::Pos2; 2],
        key: u64,
        route: Route,
    ) -> Self {
        Self::new(to, from, ends, key, route, false)
    }

    /// A stand-in for the connection from `from` to `to` that the budget left
    /// unrouted: the plain curve between its current `ends`.
    fn provisional(to: InputRef, from: OutputRef, ends: [egui::Pos2; 2], key: u64) -> Self {
        let [start, end] = ends;
        let curve = routing::bezier(start, end, CONNECTION_MIN_HANDLE);
        Self::new(to, from, ends, key, Route::Curve(curve), true)
    }

    fn new(
        to: InputRef,
        from: OutputRef,
        ends: [egui::Pos2; 2],
        key: u64,
        route: Route,
        provisional: bool,
    ) -> Self {
        let polyline = route.polyline();
        let mut hasher = KeyHasher::default();
        hash_points(&mut hasher, &polyline);
        Self {
            to,
            from,
            key,
            ends,
            provisional,
            route,
            shape: hasher.finish(),
            bounds: egui::Rect::from_points(&polyline),
            polyline,
        }
    }
}

/// What one [`RouteCache::update`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RouteUpdate {
    /// Routes computed afresh.
    pub(super) recomputed: usize,
    /// Outdated or new routes left for a later update because the budget ran out.
    pub(super) deferred: usize,
}

/// Routes of all connections, in a fixed order (by source, then target), which is
/// also the paint order.
///
/// Connections are routed one after another, each against the routes before it and
/// the stubs of all the others (see the `routing` module docs), so different
/// sources' connections avoid crossing where they can.
#[derive(Default)]
pub(in crate::editor) struct RouteCache {
    /// Routes as of the last [`update`](Self::update), in routing order.
    routes: Vec<RoutedConnection>,
    /// Fingerprint of the nodes and connections the last update saw.
    inputs: u64,
    /// The last update left nothing deferred: with the same inputs, the next one
    /// would have nothing to do.
    settled: bool,
}

impl RouteCache {
    /// The routes as of the last [`update`](Self::update), in paint order.
    pub(super) fn routes(&self) -> &[RoutedConnection] {
        &self.routes
    }

    /// Brings the routes up to date with `nodes` (placed on the canvas) and the
    /// connections of `graph`.
    ///
    /// When neither changed since an update that finished everything, returns at
    /// once (an order-independent fingerprint of both, no per-connection work).
    ///
    /// Routing runs a path search per blocked connection, so routes are kept per
    /// connection. A route's key hashes its ends and the nodes, stubs and earlier
    /// routes' shapes (polylines) within its [`routing::influence`] area; it is
    /// recomputed only when that key changes. Dragging a node reroutes only the
    /// connections near it, and those after them only if a nearby route actually
    /// changed shape, not every connection on the canvas.
    ///
    /// Recomputing is limited to `budget` per update (after at least one route),
    /// spent by priority. An outdated route whose ends didn't move and that still
    /// keeps out of every node ([`routing::still_clear`]) is fit to draw, and most
    /// come out the same anyway: those are refreshed only within the first
    /// [`REFRESH_SHARE`] of the budget. The rest goes to routes that must change
    /// (moved ends, a node in the way, new connections). Whatever is left over is
    /// [`deferred`](RouteUpdate::deferred) to the next updates: it keeps its old
    /// route if its ends are the same, otherwise it gets the plain curve between
    /// its new ends (so a wire never lags behind a dragged node). Once the layout
    /// holds still, every route catches up to exactly what routing all of them
    /// afresh would give.
    pub(super) fn update(
        &mut self,
        nodes: &[NodeWidget],
        graph: &Graph,
        budget: Duration,
    ) -> RouteUpdate {
        let inputs = fingerprint(nodes, graph);
        if self.settled && inputs == self.inputs {
            return RouteUpdate::default();
        }
        self.inputs = inputs;

        let by_id: HashMap<NodeId, &NodeWidget> =
            nodes.iter().map(|node| (node.id, node)).collect();
        let socket_pos = |node: NodeId, side: SocketDirection, index: usize| {
            by_id.get(&node).map(|node| node.socket_pos(side, index))
        };
        // Sorted by ID, so raising a node (which only reorders `nodes`) changes no key.
        let mut by_id_order: Vec<&NodeWidget> = nodes.iter().collect();
        by_id_order.sort_by_key(|node| node.id);
        let node_rects: Vec<_> = by_id_order.iter().map(|node| node.rect()).collect();

        let mut ends: Vec<_> = graph
            .connections()
            .filter_map(|(from, to)| {
                let start = socket_pos(from.node, SocketDirection::Output, from.index)?;
                let end = socket_pos(to.node, SocketDirection::Input, to.index)?;
                Some((to, from, start, end))
            })
            .collect();
        ends.sort_by_key(|&(to, from, ..)| (from, to));
        let stubs: Vec<_> = ends
            .iter()
            .flat_map(|(_, from, start, end)| {
                routing::stubs(*start, *end).map(|stub| (*from, stub))
            })
            .collect();

        // Routes of connections that are gone are dropped with `previous`.
        let mut previous: HashMap<InputRef, RoutedConnection> = std::mem::take(&mut self.routes)
            .into_iter()
            .map(|routed| (routed.to, routed))
            .collect();
        let mut routed: Vec<RoutedConnection> = Vec::with_capacity(ends.len());
        let mut update = RouteUpdate::default();
        let started = Instant::now();
        let refresh_budget = budget.mul_f32(REFRESH_SHARE);
        for &(to, from, start, end) in &ends {
            let ends = [start, end];
            let inputs = RouteInputs::gather(from, ends, &node_rects, &stubs, &routed);
            let key = inputs.key;

            let spent = |limit: Duration| update.recomputed > 0 && started.elapsed() >= limit;
            let cached = previous.remove(&to).filter(|cached| cached.from == from);
            let same_ends = |cached: &RoutedConnection| cached.ends == ends;
            let connection = match cached {
                Some(cached) if cached.key == key && !cached.provisional => cached,
                Some(cached)
                    if !cached.provisional
                        && same_ends(&cached)
                        && spent(refresh_budget)
                        && routing::still_clear(&cached.route, &inputs.obstacles) =>
                {
                    update.deferred += 1;
                    cached
                }
                cached if spent(budget) => {
                    update.deferred += 1;
                    match cached {
                        Some(cached) if same_ends(&cached) => cached,
                        _ => RoutedConnection::provisional(to, from, ends, key),
                    }
                }
                _ => {
                    update.recomputed += 1;
                    let route = routing::route(
                        start,
                        end,
                        &inputs.obstacles,
                        &inputs.wires,
                        &inputs.siblings,
                        CONNECTION_MIN_HANDLE,
                    );
                    RoutedConnection::routed(to, from, ends, key, route)
                }
            };
            routed.push(connection);
        }
        self.routes = routed;
        self.settled = update.deferred == 0;
        update
    }
}

/// What one connection's route depends on: the nodes, wires and siblings within
/// its [`routing::influence`] area, and a key hashing them with the connection's
/// ends (see [`RouteCache::update`]).
struct RouteInputs<'a> {
    obstacles: Vec<egui::Rect>,
    /// Stubs of other outputs' connections and earlier routes from other outputs.
    wires: Vec<&'a [egui::Pos2]>,
    /// Earlier routes from the same output: fanning out may share a stretch.
    siblings: Vec<&'a [egui::Pos2]>,
    key: u64,
}

impl<'a> RouteInputs<'a> {
    /// The inputs of the connection from output `from` between `ends`, given
    /// `node_rects` (sorted by node ID), the `stubs` of every connection with
    /// its output, and the connections `routed` before this one.
    fn gather(
        from: OutputRef,
        ends: [egui::Pos2; 2],
        node_rects: &[egui::Rect],
        stubs: &'a [(OutputRef, [egui::Pos2; 2])],
        routed: &'a [RoutedConnection],
    ) -> Self {
        let [start, end] = ends;
        let area = routing::influence(start, end, CONNECTION_MIN_HANDLE);
        let obstacles: Vec<_> = node_rects
            .iter()
            .copied()
            .filter(|rect| rect.intersects(area))
            .collect();
        let mut hasher = KeyHasher::default();
        hash_points(&mut hasher, &ends);
        for rect in &obstacles {
            hash_points(&mut hasher, &[rect.min, rect.max]);
        }
        let mut wires: Vec<&[egui::Pos2]> = Vec::new();
        let mut siblings: Vec<&[egui::Pos2]> = Vec::new();
        for (_, stub) in stubs.iter().filter(|(source, stub)| {
            *source != from && egui::Rect::from_two_pos(stub[0], stub[1]).intersects(area)
        }) {
            hash_points(&mut hasher, stub);
            wires.push(stub);
        }
        for earlier in routed
            .iter()
            .filter(|earlier| earlier.bounds.intersects(area))
        {
            let sibling = earlier.from == from;
            // Distinguishes a sibling from a wire of the same shape.
            sibling.hash(&mut hasher);
            earlier.shape.hash(&mut hasher);
            if sibling {
                siblings.push(&earlier.polyline);
            } else {
                wires.push(&earlier.polyline);
            }
        }
        Self {
            obstacles,
            wires,
            siblings,
            key: hasher.finish(),
        }
    }
}

/// A fingerprint of everything routes are computed from: each node's ID and rect
/// and each connection. Order-independent (a sum of per-item hashes), so it needs
/// no sorting and raising a node doesn't change it.
fn fingerprint(nodes: &[NodeWidget], graph: &Graph) -> u64 {
    let item = |hash: &dyn Fn(&mut KeyHasher)| {
        let mut hasher = KeyHasher::default();
        hash(&mut hasher);
        hasher.finish()
    };
    let nodes = nodes.iter().fold(0u64, |sum, node| {
        let rect = node.rect();
        sum.wrapping_add(item(&|hasher| {
            node.id.hash(hasher);
            hash_points(hasher, &[rect.min, rect.max]);
        }))
    });
    let connections = graph.connections().fold(0u64, |sum, (from, to)| {
        sum.wrapping_add(item(&|hasher| (to, from).hash(hasher)))
    });
    item(&|hasher| (nodes, connections).hash(hasher))
}

/// Hashes the bits of `points`, so equal layouts give equal keys.
fn hash_points(hasher: &mut KeyHasher, points: &[egui::Pos2]) {
    for point in points {
        hasher.write_u64(u64::from(point.x.to_bits()) << 32 | u64::from(point.y.to_bits()));
    }
}

/// A fast, non-cryptographic hasher for route keys (the "Fx" hash used by rustc):
/// a rotate, xor and multiply per word. The standard SipHash is built to resist
/// crafted collisions, which cache keys of a local layout don't need; here it was
/// most of the time an update spends when few routes change.
#[derive(Default)]
struct KeyHasher(u64);

impl KeyHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.add(u64::from_le_bytes(*chunk));
        }
        if !rest.is_empty() {
            let mut word = [0; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::editor::ui::std_kind;
    use v_signal_nodes_core::Node;
    use v_signal_nodes_plugin_std::{GAIN, SINK, SOURCE};

    /// A graph with its nodes placed: `ids[i]` at `pos[i]`.
    struct Layout {
        graph: Graph,
        ids: Vec<NodeId>,
        pos: Vec<egui::Pos2>,
    }

    impl Layout {
        /// Source 0 feeding sink 2 around gain 1 in the way, and source 3 feeding
        /// the gain.
        fn new() -> Self {
            let mut layout = Self {
                graph: Graph::new(),
                ids: Vec::new(),
                pos: Vec::new(),
            };
            layout.add(SOURCE, 0.0, 0.0);
            layout.add(GAIN, 250.0, 0.0);
            layout.add(SINK, 500.0, 0.0);
            layout.add(SOURCE, 0.0, 150.0);
            let ids = &layout.ids;
            let connections = [(ids[0], ids[2]), (ids[3], ids[1])];
            for (from, to) in connections {
                layout
                    .graph
                    .connect(OutputRef::new(from, 0), InputRef::new(to, 0))
                    .unwrap();
            }
            layout
        }

        fn add(&mut self, type_id: &str, x: f32, y: f32) {
            let spec = Arc::new(std_kind(type_id).spec().clone());
            self.ids.push(self.graph.add(Node::new(spec)));
            self.pos.push(egui::pos2(x, y));
        }

        fn nodes(&self) -> Vec<NodeWidget<'_>> {
            self.ids
                .iter()
                .zip(&self.pos)
                .map(|(&id, &pos)| NodeWidget::new(id, self.graph.node(id).unwrap(), pos))
                .collect()
        }

        fn update(&self, cache: &mut RouteCache, budget: Duration) -> RouteUpdate {
            cache.update(&self.nodes(), &self.graph, budget)
        }
    }

    fn routes(cache: &RouteCache) -> Vec<(InputRef, Route)> {
        cache
            .routes()
            .iter()
            .map(|routed| (routed.to, routed.route.clone()))
            .collect()
    }

    #[test]
    fn routes_are_reused_until_something_nearby_moves() {
        let mut layout = Layout::new();
        let mut cache = RouteCache::default();

        let first = layout.update(&mut cache, Duration::MAX);
        assert_eq!(first.recomputed, 2);
        assert!(matches!(cache.routes()[0].route, Route::Path(_)));
        let routed = routes(&cache);

        // Nothing changed: nothing is recomputed.
        let again = layout.update(&mut cache, Duration::MAX);
        assert_eq!(again, RouteUpdate::default());
        assert_eq!(routes(&cache), routed);

        // A node far outside every influence area doesn't matter either.
        layout.add(GAIN, 5_000.0, 5_000.0);
        assert_eq!(layout.update(&mut cache, Duration::MAX).recomputed, 0);

        // Moving the gain in the way reroutes.
        layout.pos[1].y += 40.0;
        assert!(layout.update(&mut cache, Duration::MAX).recomputed > 0);
    }

    #[test]
    fn an_exhausted_budget_defers_routes_to_later_updates() {
        let layout = Layout::new();
        let mut full = RouteCache::default();
        layout.update(&mut full, Duration::MAX);

        let mut cache = RouteCache::default();
        let first = layout.update(&mut cache, Duration::ZERO);
        // At least one route per update, the other one stands in as the curve.
        assert_eq!(
            first,
            RouteUpdate {
                recomputed: 1,
                deferred: 1
            }
        );
        assert_eq!(cache.routes().len(), 2);
        assert!(cache.routes()[1].provisional);

        let second = layout.update(&mut cache, Duration::ZERO);
        assert_eq!(second.deferred, 0);
        assert_eq!(routes(&cache), routes(&full));
    }

    #[test]
    fn budgeted_updates_catch_up_to_routing_everything_afresh() {
        let mut layout = Layout::new();
        let mut cache = RouteCache::default();
        layout.update(&mut cache, Duration::MAX);

        // Moves in small budgets, as during a drag: outdated routes may linger.
        for step in 0..10 {
            layout.pos[1].y = 20.0 * step as f32;
            layout.update(&mut cache, Duration::ZERO);
        }
        while layout.update(&mut cache, Duration::ZERO).deferred > 0 {}

        let mut fresh = RouteCache::default();
        layout.update(&mut fresh, Duration::MAX);
        assert_eq!(routes(&cache), routes(&fresh));
        // Settled: the next update returns at once.
        assert_eq!(
            layout.update(&mut cache, Duration::ZERO),
            RouteUpdate::default()
        );
    }

    #[test]
    fn a_deferred_connection_follows_its_moved_ends() {
        let mut layout = Layout::new();
        let mut cache = RouteCache::default();
        layout.update(&mut cache, Duration::MAX);

        // Move both connections' sources; with no budget only one is rerouted.
        layout.pos[0].y += 30.0;
        layout.pos[3].y += 30.0;
        let update = layout.update(&mut cache, Duration::ZERO);
        assert_eq!(update.deferred, 1);

        // The deferred one is drawn between its new ends, not left behind.
        let nodes = layout.nodes();
        let node = |id: NodeId| nodes.iter().find(|node| node.id == id).unwrap();
        for routed in cache.routes() {
            let start =
                node(routed.from.node).socket_pos(SocketDirection::Output, routed.from.index);
            let end = node(routed.to.node).socket_pos(SocketDirection::Input, routed.to.index);
            assert_eq!(routed.polyline.first(), Some(&start));
            assert_eq!(routed.polyline.last(), Some(&end));
        }
    }

    #[test]
    fn removed_connections_are_dropped() {
        let mut layout = Layout::new();
        let mut cache = RouteCache::default();
        layout.update(&mut cache, Duration::MAX);

        layout.graph.disconnect(InputRef::new(layout.ids[2], 0));
        layout.update(&mut cache, Duration::MAX);

        assert_eq!(cache.routes().len(), 1);
        assert_eq!(cache.routes()[0].to, InputRef::new(layout.ids[1], 0));
    }
}

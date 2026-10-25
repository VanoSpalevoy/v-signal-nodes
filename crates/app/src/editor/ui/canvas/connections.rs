//! The routes of all connections, kept up to date across frames.
//!
//! [`RouteCache`] lives in the app (it is derived data, never saved) rather than in
//! egui memory, so it is plain Rust that unit tests can drive without a UI.

use eframe::egui;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use super::CONNECTION_MIN_HANDLE;
use crate::editor::ui::routing::{self, Route};
use crate::editor::{NodeWidget, SocketRef};

/// Time per frame for recomputing connection routes (see [`RouteCache::update`]);
/// the rest wait for the next frames.
pub(super) const ROUTING_BUDGET: Duration = Duration::from_millis(4);
/// Share of [`ROUTING_BUDGET`] that may go to refreshing routes that are outdated
/// but still fit to draw; the rest is kept for routes that must change.
const REFRESH_SHARE: f32 = 0.5;

/// One connection's route, with what later connections need to route around it.
pub(super) struct RoutedConnection {
    /// The input socket (the connection's key in `VSignalNodesApp::connections`).
    pub(super) to: SocketRef,
    /// The output socket feeding `to`.
    pub(super) from: SocketRef,
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
    fn new(
        (to, from): (SocketRef, SocketRef),
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

    /// Brings the routes up to date with `nodes` and `connections`
    /// (`VSignalNodesApp::connections`: input → output).
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
        connections: &HashMap<SocketRef, SocketRef>,
        budget: Duration,
    ) -> RouteUpdate {
        let inputs = fingerprint(nodes, connections);
        if self.settled && inputs == self.inputs {
            return RouteUpdate::default();
        }
        self.inputs = inputs;

        let by_id: HashMap<usize, &NodeWidget> = nodes.iter().map(|node| (node.id, node)).collect();
        let socket_pos = |socket: SocketRef| {
            by_id
                .get(&socket.node_id)
                .map(|node| node.socket_pos(socket.side, socket.index))
        };
        // Sorted by ID, so raising a node (which only reorders `nodes`) changes no key.
        let mut node_rects: Vec<_> = nodes.iter().map(|node| (node.id, node.rect())).collect();
        node_rects.sort_by_key(|(id, _)| *id);

        let mut ends: Vec<_> = connections
            .iter()
            .filter_map(|(to, from)| Some((*to, *from, socket_pos(*from)?, socket_pos(*to)?)))
            .collect();
        ends.sort_by_key(|&(to, from, ..)| (from, to));
        let stubs: Vec<_> = ends
            .iter()
            .flat_map(|(_, from, start, end)| {
                routing::stubs(*start, *end).map(|stub| (*from, stub))
            })
            .collect();

        // Routes of connections that are gone are dropped with `previous`.
        let mut previous: HashMap<SocketRef, RoutedConnection> = std::mem::take(&mut self.routes)
            .into_iter()
            .map(|routed| (routed.to, routed))
            .collect();
        let mut routed: Vec<RoutedConnection> = Vec::with_capacity(ends.len());
        let mut update = RouteUpdate::default();
        let started = Instant::now();
        let refresh_budget = budget.mul_f32(REFRESH_SHARE);
        for &(to, from, start, end) in &ends {
            let area = routing::influence(start, end, CONNECTION_MIN_HANDLE);
            let obstacles: Vec<_> = node_rects
                .iter()
                .map(|(_, rect)| *rect)
                .filter(|rect| rect.intersects(area))
                .collect();
            let mut hasher = KeyHasher::default();
            hash_points(&mut hasher, &[start, end]);
            for rect in &obstacles {
                hash_points(&mut hasher, &[rect.min, rect.max]);
            }
            // Wires from the same output are siblings: fanning out may share a stretch.
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
                // Distinguishes a sibling from a wire of the same shape.
                (earlier.from == from).hash(&mut hasher);
                earlier.shape.hash(&mut hasher);
                if earlier.from == from {
                    siblings.push(&earlier.polyline);
                } else {
                    wires.push(&earlier.polyline);
                }
            }
            let key = hasher.finish();

            let spent = |limit: Duration| update.recomputed > 0 && started.elapsed() >= limit;
            let cached = previous.remove(&to).filter(|cached| cached.from == from);
            let same_ends = |cached: &RoutedConnection| cached.ends == [start, end];
            let connection = match cached {
                Some(cached) if cached.key == key && !cached.provisional => cached,
                Some(cached)
                    if !cached.provisional
                        && same_ends(&cached)
                        && spent(refresh_budget)
                        && routing::still_clear(&cached.route, &obstacles) =>
                {
                    update.deferred += 1;
                    cached
                }
                cached if spent(budget) => {
                    update.deferred += 1;
                    match cached {
                        Some(cached) if same_ends(&cached) => cached,
                        _ => {
                            let curve = routing::bezier(start, end, CONNECTION_MIN_HANDLE);
                            let route = Route::Curve(curve);
                            RoutedConnection::new((to, from), [start, end], key, route, true)
                        }
                    }
                }
                _ => {
                    update.recomputed += 1;
                    let route = routing::route(
                        start,
                        end,
                        &obstacles,
                        &wires,
                        &siblings,
                        CONNECTION_MIN_HANDLE,
                    );
                    RoutedConnection::new((to, from), [start, end], key, route, false)
                }
            };
            routed.push(connection);
        }
        self.routes = routed;
        self.settled = update.deferred == 0;
        update
    }
}

/// A fingerprint of everything routes are computed from: each node's ID and rect
/// and each connection. Order-independent (a sum of per-item hashes), so it needs
/// no sorting and raising a node doesn't change it.
fn fingerprint(nodes: &[NodeWidget], connections: &HashMap<SocketRef, SocketRef>) -> u64 {
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
    let connections = connections.iter().fold(0u64, |sum, (to, from)| {
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
    use super::*;
    use crate::editor::{NodeConfig, ui::std_kind};
    use v_signal_nodes_plugin_std::{GAIN, SINK, SOURCE};

    fn node(id: usize, type_id: &str, x: f32, y: f32) -> NodeWidget {
        NodeWidget::new(
            id,
            NodeConfig::for_kind(&std_kind(type_id)),
            egui::pos2(x, y),
        )
    }

    /// Source 0 feeding sink 2 around gain 1 in the way, and source 3 feeding the gain.
    fn layout() -> (Vec<NodeWidget>, HashMap<SocketRef, SocketRef>) {
        let nodes = vec![
            node(0, SOURCE, 0.0, 0.0),
            node(1, GAIN, 250.0, 0.0),
            node(2, SINK, 500.0, 0.0),
            node(3, SOURCE, 0.0, 150.0),
        ];
        let connections = HashMap::from([
            (SocketRef::input(2, 0), SocketRef::output(0, 0)),
            (SocketRef::input(1, 0), SocketRef::output(3, 0)),
        ]);
        (nodes, connections)
    }

    fn routes(cache: &RouteCache) -> Vec<(SocketRef, Route)> {
        cache
            .routes()
            .iter()
            .map(|routed| (routed.to, routed.route.clone()))
            .collect()
    }

    #[test]
    fn routes_are_reused_until_something_nearby_moves() {
        let (mut nodes, connections) = layout();
        let mut cache = RouteCache::default();

        let first = cache.update(&nodes, &connections, Duration::MAX);
        assert_eq!(first.recomputed, 2);
        assert!(matches!(cache.routes()[0].route, Route::Path(_)));
        let routed = routes(&cache);

        // Nothing changed: nothing is recomputed.
        let again = cache.update(&nodes, &connections, Duration::MAX);
        assert_eq!(again, RouteUpdate::default());
        assert_eq!(routes(&cache), routed);

        // A node far outside every influence area doesn't matter either.
        nodes.push(node(4, GAIN, 5_000.0, 5_000.0));
        assert_eq!(
            cache.update(&nodes, &connections, Duration::MAX).recomputed,
            0
        );

        // Moving the gain in the way reroutes.
        nodes[1].pos.y += 40.0;
        assert!(cache.update(&nodes, &connections, Duration::MAX).recomputed > 0);
    }

    #[test]
    fn an_exhausted_budget_defers_routes_to_later_updates() {
        let (nodes, connections) = layout();
        let mut full = RouteCache::default();
        full.update(&nodes, &connections, Duration::MAX);

        let mut cache = RouteCache::default();
        let first = cache.update(&nodes, &connections, Duration::ZERO);
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

        let second = cache.update(&nodes, &connections, Duration::ZERO);
        assert_eq!(second.deferred, 0);
        assert_eq!(routes(&cache), routes(&full));
    }

    #[test]
    fn budgeted_updates_catch_up_to_routing_everything_afresh() {
        let (mut nodes, connections) = layout();
        let mut cache = RouteCache::default();
        cache.update(&nodes, &connections, Duration::MAX);

        // Moves in small budgets, as during a drag: outdated routes may linger.
        for step in 0..10 {
            nodes[1].pos.y = 20.0 * step as f32;
            cache.update(&nodes, &connections, Duration::ZERO);
        }
        while cache.update(&nodes, &connections, Duration::ZERO).deferred > 0 {}

        let mut fresh = RouteCache::default();
        fresh.update(&nodes, &connections, Duration::MAX);
        assert_eq!(routes(&cache), routes(&fresh));
        // Settled: the next update returns at once.
        assert_eq!(
            cache.update(&nodes, &connections, Duration::ZERO),
            RouteUpdate::default()
        );
    }

    #[test]
    fn a_deferred_connection_follows_its_moved_ends() {
        let (mut nodes, connections) = layout();
        let mut cache = RouteCache::default();
        cache.update(&nodes, &connections, Duration::MAX);

        // Move both connections' sources; with no budget only one is rerouted.
        nodes[0].pos.y += 30.0;
        nodes[3].pos.y += 30.0;
        let update = cache.update(&nodes, &connections, Duration::ZERO);
        assert_eq!(update.deferred, 1);

        // The deferred one is drawn between its new ends, not left behind.
        for routed in cache.routes() {
            let pos = |socket: SocketRef| {
                let node = nodes.iter().find(|node| node.id == socket.node_id).unwrap();
                node.socket_pos(socket.side, socket.index)
            };
            assert_eq!(routed.polyline.first(), Some(&pos(routed.from)));
            assert_eq!(routed.polyline.last(), Some(&pos(routed.to)));
        }
    }

    #[test]
    fn removed_connections_are_dropped() {
        let (nodes, mut connections) = layout();
        let mut cache = RouteCache::default();
        cache.update(&nodes, &connections, Duration::MAX);

        connections.remove(&SocketRef::input(2, 0));
        cache.update(&nodes, &connections, Duration::MAX);

        assert_eq!(cache.routes().len(), 1);
        assert_eq!(cache.routes()[0].to, SocketRef::input(1, 0));
    }
}

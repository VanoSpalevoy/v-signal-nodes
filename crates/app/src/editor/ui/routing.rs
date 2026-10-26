//! Routing connections around nodes.
//!
//! [`route`] first tries the plain S-shaped Bézier curve from the output to the
//! input. If that curve passes through any node, or crosses another connection
//! (a *wire*), it looks for an orthogonal path around the nodes instead:
//!
//! 1. Leave the output with a horizontal stub of [`STUB`] and enter the input the
//!    same way, so connections always run out of the right edge and into the left.
//! 2. Search between the two stub ends on a grid with A*. Cells closer than
//!    [`MIN_CLEARANCE`] to a node are blocked; cells closer than [`CLEARANCE`] are
//!    allowed but cost extra, so paths keep their distance where there is room and
//!    still squeeze between closely packed nodes. Cells on a wire cost
//!    [`WIRE_COST`], so paths go around other connections instead of crossing or
//!    running along them where they can. Every turn costs extra, so paths have as
//!    few bends as possible; arriving at the input other than left-to-right costs
//!    a turn too.
//! 3. Merge collinear cells into corner points. The canvas draws the result with
//!    rounded corners.
//!
//! The grid is aligned so both stub ends are exactly grid points: the cell size is
//! adjusted per connection (close to [`CELL`]) so the distance between them is a
//! whole number of cells. That keeps the final segments straight, without jogs.
//! The one exception: ends less than half a cell apart on an axis (but not
//! aligned) can't both be grid points, so the last grid point is joined to the
//! input's stub end with a short orthogonal jog.
//!
//! A curve that only crosses wires (no nodes) is replaced by the path only when
//! the path crosses fewer of them; when crossings can't be avoided, the curve
//! looks better.
//!
//! If no path exists inside the search area, the plain curve is used. Everything
//! is in canvas coordinates.
//!
//! # Cost
//!
//! Routes are recomputed on every frame of a drag, so a search is kept cheap: its
//! heuristic counts the turns a path can't avoid ([`min_turns`]) besides the
//! distance, ties go to the state closest to the goal, its buffers are reused
//! across searches (see `Scratch`), a search that runs long
//! checks once with a flood fill that the input can be reached at all
//! ([`REACHABILITY_CHECK_AFTER`]), and none expands more than [`MAX_EXPANDED`]
//! states. Giving up means the plain curve.
//!
//! A route depends only on its ends and the nodes and wires within [`influence`],
//! so callers can cache routes per connection and recompute just the ones a move
//! affects.
//!
//! # Wires
//!
//! Connections are routed one after another, each against the wires of those
//! routed before it ([`Route::polyline`]) and the [`stubs`] of all the others, so
//! a path never runs through the spot where another connection enters its input.
//! Wires from the same output (*siblings*) are passed separately: connections
//! fanning out of one output may share a stretch, so a path runs along a sibling
//! from the shared output for free, but stepping onto one from elsewhere costs
//! [`WIRE_COST`] like crossing a wire. A curve that crosses a sibling is replaced
//! by a path that crosses fewer, which usually branches off the sibling's trunk.

use eframe::egui::{Pos2, Rect, Vec2, vec2};
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Length of the horizontal run out of an output and into an input.
const STUB: f32 = 20.0;
/// Distance a routed path keeps from every node when there is room.
const CLEARANCE: f32 = 12.0;
/// Distance a routed path always keeps from every node.
const MIN_CLEARANCE: f32 = 4.0;
/// Nominal grid cell size of the path search.
const CELL: f32 = 10.0;
/// How far beyond the two endpoints the search may go around obstacles.
const SEARCH_MARGIN: f32 = 240.0;
/// Upper bound on grid cells per search, to keep the cost of one route bounded.
const MAX_CELLS: usize = 120_000;
/// Upper bound on search states A* expands before giving up (the curve is used
/// then). A connection with no way through would otherwise explore every state of
/// the search area (up to 4 × [`MAX_CELLS`]) on each layout change.
const MAX_EXPANDED: usize = 40_000;
/// Expanded states after which A* checks once that the goal can be reached at
/// all (a cheap flood fill, see [`Grid::connected`]), so an end boxed in by nodes
/// doesn't cost a search of the whole area. Typical searches finish well before.
const REACHABILITY_CHECK_AFTER: usize = 2_000;
/// Points sampled along the Bézier curve when testing it against nodes.
const CURVE_SAMPLES: usize = 48;
/// Path cost of one grid step.
const STEP_COST: u32 = 10;
/// Extra path cost of a 90° turn.
const TURN_COST: u32 = 50;
/// Extra path cost of a grid step closer than [`CLEARANCE`] to a node.
const NEAR_COST: u32 = 30;
/// Extra path cost of a grid step onto a wire: crossing one costs about this
/// much, running along one this much per step. Worth a detour of a few turns.
const WIRE_COST: u32 = 150;

/// How a connection is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Route {
    /// Cubic Bézier control points: start, two handles, end.
    Curve([Pos2; 4]),
    /// Orthogonal polyline from the output to the input, corners only.
    Path(Vec<Pos2>),
}

impl Route {
    /// The route as a polyline (a curve is sampled), to route other connections
    /// against it as a wire.
    pub(super) fn polyline(&self) -> Vec<Pos2> {
        match self {
            Self::Curve(curve) => sample(curve),
            Self::Path(points) => points.clone(),
        }
    }
}

/// The horizontal stubs out of `start` (an output) and into `end` (an input) that
/// every route of that connection begins and ends with, as two-point wires.
pub(super) fn stubs(start: Pos2, end: Pos2) -> [[Pos2; 2]; 2] {
    [
        [start, start + vec2(STUB, 0.0)],
        [end - vec2(STUB, 0.0), end],
    ]
}

/// Control points of the plain connection curve from `start` (an output) to `end`
/// (an input): horizontal handles at least `min_handle` long, so the curve leaves
/// the output rightwards and enters the input from the left.
pub(super) fn bezier(start: Pos2, end: Pos2, min_handle: f32) -> [Pos2; 4] {
    let handle = ((end.x - start.x).abs() * 0.5).max(min_handle);
    [
        start,
        start + vec2(handle, 0.0),
        end - vec2(handle, 0.0),
        end,
    ]
}

/// Everything [`route`] looks at for a connection from `start` to `end`: the plain
/// curve's control points and the path search area, plus clearance. Conservative.
///
/// Obstacles and wires that don't intersect it can't change the route, so they can
/// be left out of `route`'s arguments, and the route only needs recomputing when
/// its ends or the obstacles and wires inside move.
pub(super) fn influence(start: Pos2, end: Pos2, min_handle: f32) -> Rect {
    // The search area reaches SEARCH_MARGIN beyond the stub ends, plus up to one
    // (stretched, < 2 × CELL) cell of rounding; the curve stays inside its control
    // points' hull, whose handles grow with the ends' distance; obstacles matter up
    // to CLEARANCE away.
    let search = Rect::from_two_pos(start, end).expand(SEARCH_MARGIN + STUB + 2.0 * CELL);
    let curve = Rect::from_points(&bezier(start, end, min_handle));
    search.union(curve).expand(CLEARANCE)
}

/// How to draw the connection from `start` to `end`, avoiding `obstacles` (node
/// rects, including the two connected nodes) and, where possible, crossing
/// `wires` (polylines of other outputs' connections) and `siblings` (polylines
/// of connections from the same output), see the module docs. Crossing fewer
/// wires comes first, then fewer siblings. Obstacles, wires and siblings outside
/// [`influence`] may be left out.
pub(super) fn route(
    start: Pos2,
    end: Pos2,
    obstacles: &[Rect],
    wires: &[&[Pos2]],
    siblings: &[&[Pos2]],
    min_handle: f32,
) -> Route {
    let all_crossings = |line: &[Pos2]| (crossings(line, wires), crossings(line, siblings));
    let curve = bezier(start, end, min_handle);
    let hits_node = curve_hits(&curve, obstacles);
    let curve_crossings = all_crossings(&sample(&curve));
    if !hits_node && curve_crossings == (0, 0) {
        return Route::Curve(curve);
    }
    match find_path(start, end, obstacles, wires, siblings) {
        Some(path) if hits_node || all_crossings(&path) < curve_crossings => Route::Path(path),
        _ => Route::Curve(curve),
    }
}

/// Whether `route` (computed earlier for the same ends) still keeps out of every
/// node in `obstacles`: a cheap check, no search. A route that passes is fit to
/// draw, though [`route`] might find a better one now (a node in the way may have
/// moved off, or wires moved).
pub(super) fn still_clear(route: &Route, obstacles: &[Rect]) -> bool {
    match route {
        Route::Curve(curve) => !curve_hits(curve, obstacles),
        // Segments are orthogonal, so each one's bounds is the segment itself.
        // Strictly inside only: the stubs start on their nodes' edges.
        Route::Path(points) => !points.windows(2).any(|segment| {
            let bounds = Rect::from_two_pos(segment[0], segment[1]);
            obstacles.iter().any(|rect| {
                bounds.min.x < rect.max.x
                    && bounds.max.x > rect.min.x
                    && bounds.min.y < rect.max.y
                    && bounds.max.y > rect.min.y
            })
        }),
    }
}

/// The curve as a polyline of [`CURVE_SAMPLES`] segments.
fn sample(curve: &[Pos2; 4]) -> Vec<Pos2> {
    (0..=CURVE_SAMPLES)
        .map(|i| cubic(curve, i as f32 / CURVE_SAMPLES as f32))
        .collect()
}

/// How many times the polyline `line` crosses `wires`. Only proper crossings
/// count: touching or running along a wire doesn't.
fn crossings(line: &[Pos2], wires: &[&[Pos2]]) -> usize {
    line.windows(2)
        .map(|a| {
            wires
                .iter()
                .flat_map(|wire| wire.windows(2))
                .filter(|b| segments_cross([a[0], a[1]], [b[0], b[1]]))
                .count()
        })
        .sum()
}

/// Whether segments `a` and `b` cross at a point strictly inside both.
fn segments_cross(a: [Pos2; 2], b: [Pos2; 2]) -> bool {
    // Which side of the line through `line` the point is on (sign of the cross product).
    let side = |line: [Pos2; 2], point: Pos2| {
        let (d, p) = (line[1] - line[0], point - line[0]);
        d.x * p.y - d.y * p.x
    };
    side(b, a[0]) * side(b, a[1]) < 0.0 && side(a, b[0]) * side(a, b[1]) < 0.0
}

fn cubic(points: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let [p0, p1, p2, p3] = points.map(Pos2::to_vec2);
    (p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t))
        .to_pos2()
}

/// Whether the curve passes through a node. The ends are skipped: they sit on
/// the connected nodes' edges by design.
fn curve_hits(curve: &[Pos2; 4], obstacles: &[Rect]) -> bool {
    (1..CURVE_SAMPLES)
        .map(|i| i as f32 / CURVE_SAMPLES as f32)
        .filter(|t| (0.04..=0.96).contains(t))
        .map(|t| cubic(curve, t))
        .any(|point| obstacles.iter().any(|rect| strictly_inside(point, *rect)))
}

/// Whether `point` is inside `rect`, not on its border.
fn strictly_inside(point: Pos2, rect: Rect) -> bool {
    point.x > rect.min.x && point.x < rect.max.x && point.y > rect.min.y && point.y < rect.max.y
}

/// Grid directions: right, left, down, up.
const DIRECTIONS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const RIGHT: usize = 0;
const LEFT: usize = 1;

/// A lower bound on the turns (each costing [`TURN_COST`]) a path heading
/// `direction` still needs to reach a goal `(di, dj)` cells away, counting the
/// penalty for arriving other than heading right as a turn. Part of the A*
/// heuristic: never too high, so paths stay the cheapest; without it, A* explores
/// every path that is as short as the best one before paying for its turns.
fn min_turns(di: i32, dj: i32, direction: usize) -> u32 {
    let horizontal = direction == RIGHT || direction == LEFT;
    match (di, dj) {
        (0, 0) => 0,
        // Another row: turn to vertical (if heading horizontally), then turn back
        // to the right or arrive heading elsewhere.
        (_, dj) if dj != 0 => {
            if horizontal {
                2
            } else {
                1
            }
        }
        // Same row ahead: heading right needs nothing, left must turn around.
        (di, _) if di > 0 => match direction {
            RIGHT => 0,
            LEFT => 2,
            _ => 1,
        },
        // Same row behind: going left and arriving needs at least a turn,
        // heading right must turn around first.
        _ => {
            if direction == RIGHT {
                2
            } else {
                1
            }
        }
    }
}

/// Orthogonal path around `obstacles`, as corner points from `start` to `end`;
/// `None` if the search area has no way through.
fn find_path(
    start: Pos2,
    end: Pos2,
    obstacles: &[Rect],
    wires: &[&[Pos2]],
    siblings: &[&[Pos2]],
) -> Option<Vec<Pos2>> {
    let from = start + vec2(STUB, 0.0);
    let to = end - vec2(STUB, 0.0);
    SCRATCH.with_borrow_mut(|scratch| {
        let grid = Grid::new(from, to, obstacles, wires, siblings, &mut scratch.flags)?;
        let cells = grid.search(&mut scratch.search)?;

        let mut points = vec![start];
        points.extend(cells.into_iter().map(|cell| grid.pos(cell)));
        // Usually the last grid point is `to` already; if not (see the module docs),
        // join it with an orthogonal jog instead of a diagonal.
        let last = grid.pos(grid.goal);
        points.extend([Pos2::new(last.x, to.y), to, end]);
        Some(corners(points))
    })
}

/// Drops points that lie on a straight line between their neighbors.
fn corners(points: Vec<Pos2>) -> Vec<Pos2> {
    let mut result: Vec<Pos2> = Vec::with_capacity(points.len());
    for point in points {
        if result.last() == Some(&point) {
            continue;
        }
        if let [.., a, b] = result[..] {
            let collinear = (a.x == b.x && b.x == point.x) || (a.y == b.y && b.y == point.y);
            if collinear {
                result.pop();
            }
        }
        result.push(point);
    }
    result
}

/// Grid point flag: closer than [`MIN_CLEARANCE`] to a node, never on a path.
const BLOCKED: u8 = 1;
/// Grid point flag: closer than [`CLEARANCE`] to a node, allowed but costs [`NEAR_COST`].
const NEAR: u8 = 2;
/// Grid point flag: on a wire, allowed but costs [`WIRE_COST`].
const WIRE: u8 = 4;
/// Grid point flag: on a sibling (a wire from the same output). Free to run along,
/// but stepping onto it from a point off it costs [`WIRE_COST`].
const SIBLING: u8 = 8;

/// Buffers reused by every path search on this thread.
///
/// A search area can have [`MAX_CELLS`] grid points and four times as many search
/// states; allocating and initializing that for every route (and connections are
/// rerouted on every frame of a drag) would cost more than many searches do.
#[derive(Default)]
struct Scratch {
    /// [`BLOCKED`] / [`NEAR`] / [`WIRE`] / [`SIBLING`] flags of the current grid's points.
    flags: Vec<u8>,
    search: SearchBuffers,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::default();
}

/// Per-state bookkeeping of the A* in [`Grid::search`].
///
/// A state's entry is valid only when its `seen` equals `generation`, so starting
/// a search is just bumping `generation`, not refilling the buffers.
#[derive(Default)]
struct SearchBuffers {
    /// Per search state; see [`StateEntry`].
    states: Vec<StateEntry>,
    generation: u32,
    /// Open set as `(estimated total cost, estimated remaining cost, state)`: of
    /// equally promising states the one closest to the goal comes first, so A*
    /// heads for the goal instead of widening across a plateau of equal estimates.
    open: BinaryHeap<Reverse<(u32, u32, u32)>>,
    /// Per grid point: reached by this search's flood fill when equal to
    /// `generation` (see [`Grid::connected`]).
    flooded: Vec<u32>,
    /// Flood fill queue of grid point indices.
    queue: Vec<u32>,
}

/// What A* knows about one search state. The fields sit together (not in one
/// array each) because they are always read together: one cache miss per
/// neighbor instead of three, in the hottest loop of routing.
#[derive(Clone, Copy, Default)]
struct StateEntry {
    /// The search `generation` this entry belongs to.
    seen: u32,
    /// Best known path cost to the state.
    cost: u32,
    /// The state that cost was reached from, [`NO_STATE`] for the start.
    came_from: u32,
}

/// `came_from` of the start state.
const NO_STATE: u32 = u32::MAX;

impl SearchBuffers {
    /// Readies the buffers for a search over `points` grid points (four states
    /// each), all unseen.
    fn start(&mut self, points: usize) {
        let states = points * 4;
        if self.states.len() < states {
            self.states.resize(states, StateEntry::default());
        }
        if self.flooded.len() < points {
            self.flooded.resize(points, 0);
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Wrapped around: old stamps could look current again.
            self.states.fill(StateEntry::default());
            self.flooded.fill(0);
            self.generation = 1;
        }
        self.open.clear();
    }

    /// Best known cost of `state` in this search.
    fn cost(&self, state: usize) -> u32 {
        let entry = self.states[state];
        if entry.seen == self.generation {
            entry.cost
        } else {
            u32::MAX
        }
    }

    fn set(&mut self, state: usize, cost: u32, came_from: u32) {
        self.states[state] = StateEntry {
            seen: self.generation,
            cost,
            came_from,
        };
    }
}

/// Search grid for one connection. Grid point `(i, j)` is at `origin + (i, j) * cell`;
/// `origin` is the output's stub end, and the input's stub end is also a grid point.
struct Grid<'a> {
    origin: Pos2,
    cell: Vec2,
    /// Index range of grid points, inclusive.
    min: (i32, i32),
    columns: usize,
    rows: usize,
    /// [`BLOCKED`] / [`NEAR`] / [`WIRE`] / [`SIBLING`] flags per grid point.
    flags: &'a mut [u8],
    goal: (i32, i32),
}

impl<'a> Grid<'a> {
    /// The grid between stub ends `from` and `to`, with its flags in `flags`
    /// (reused, see [`Scratch`]). `None` if the area has more than [`MAX_CELLS`] points.
    fn new(
        from: Pos2,
        to: Pos2,
        obstacles: &[Rect],
        wires: &[&[Pos2]],
        siblings: &[&[Pos2]],
        flags: &'a mut Vec<u8>,
    ) -> Option<Self> {
        let delta = to - from;
        let cell_size = |distance: f32| {
            let steps = (distance / CELL).round();
            if steps == 0.0 {
                CELL
            } else {
                (distance / steps).abs()
            }
        };
        let cell = vec2(cell_size(delta.x), cell_size(delta.y));
        let goal = (
            (delta.x / cell.x).round() as i32,
            (delta.y / cell.y).round() as i32,
        );

        // Search area: around both stub ends, but not into negative coordinates
        // (unless an endpoint itself is there).
        let floor = from.min(to).min(Pos2::ZERO);
        let lo = (from.min(to) - Vec2::splat(SEARCH_MARGIN)).max(floor);
        let hi = from.max(to) + Vec2::splat(SEARCH_MARGIN);
        // Size check in floats first: for ends very far apart the integer cell
        // counts below would saturate and overflow.
        let span = (hi - lo) / cell + Vec2::splat(2.0);
        if span.x * span.y > MAX_CELLS as f32 {
            return None;
        }
        let min = (
            ((lo.x - from.x) / cell.x).floor() as i32,
            ((lo.y - from.y) / cell.y).floor() as i32,
        );
        let max = (
            ((hi.x - from.x) / cell.x).ceil() as i32,
            ((hi.y - from.y) / cell.y).ceil() as i32,
        );
        let columns = (max.0 - min.0 + 1) as usize;
        let rows = (max.1 - min.1 + 1) as usize;
        if columns * rows > MAX_CELLS {
            return None;
        }

        flags.clear();
        flags.resize(columns * rows, 0);
        let mut grid = Self {
            origin: from,
            cell,
            min,
            columns,
            rows,
            flags,
            goal,
        };
        for obstacle in obstacles {
            grid.mark_rect(obstacle.expand(CLEARANCE), NEAR);
            grid.mark_rect(obstacle.expand(MIN_CLEARANCE), BLOCKED);
        }
        grid.mark_wires(wires, WIRE);
        grid.mark_wires(siblings, SIBLING);
        // The stub ends sit right next to their nodes; they must stay reachable.
        // The output's stub end is on every sibling's stub too: keeping that flag
        // lets a path follow a sibling from the start for free.
        for end in [(0, 0), goal] {
            if let Some(index) = grid.index(end) {
                grid.flags[index] &= SIBLING;
            }
        }
        Some(grid)
    }

    fn is(&self, index: usize, flag: u8) -> bool {
        self.flags[index] & flag != 0
    }

    /// Sets `flag` on the grid points nearest to `wires`. Samples every half cell,
    /// so the marked points of a wire are 8-connected and a path moving in steps of
    /// one cell can't slip through a diagonal wire without stepping on it.
    fn mark_wires(&mut self, wires: &[&[Pos2]], flag: u8) {
        let spacing = self.cell.x.min(self.cell.y) * 0.5;
        for segment in wires.iter().flat_map(|wire| wire.windows(2)) {
            let samples = (segment[0].distance(segment[1]) / spacing).ceil().max(1.0) as usize;
            for k in 0..=samples {
                let point = segment[0].lerp(segment[1], k as f32 / samples as f32);
                let cell = (
                    ((point.x - self.origin.x) / self.cell.x).round() as i32,
                    ((point.y - self.origin.y) / self.cell.y).round() as i32,
                );
                if let Some(index) = self.index(cell) {
                    self.flags[index] |= flag;
                }
            }
        }
    }

    /// Sets `flag` on the grid points inside `rect`, borders included.
    fn mark_rect(&mut self, rect: Rect, flag: u8) {
        let last = (
            self.min.0 + self.columns as i32 - 1,
            self.min.1 + self.rows as i32 - 1,
        );
        let i0 = (((rect.min.x - self.origin.x) / self.cell.x).ceil() as i32).max(self.min.0);
        let i1 = (((rect.max.x - self.origin.x) / self.cell.x).floor() as i32).min(last.0);
        let j0 = (((rect.min.y - self.origin.y) / self.cell.y).ceil() as i32).max(self.min.1);
        let j1 = (((rect.max.y - self.origin.y) / self.cell.y).floor() as i32).min(last.1);
        if i0 > i1 {
            return;
        }
        for j in j0..=j1 {
            let row = self.index((i0, j)).expect("clamped to the grid");
            for cell in &mut self.flags[row..=row + (i1 - i0) as usize] {
                *cell |= flag;
            }
        }
    }

    fn index(&self, (i, j): (i32, i32)) -> Option<usize> {
        let x = usize::try_from(i - self.min.0).ok()?;
        let y = usize::try_from(j - self.min.1).ok()?;
        (x < self.columns && y < self.rows).then_some(y * self.columns + x)
    }

    fn cell_of(&self, index: usize) -> (i32, i32) {
        (
            (index % self.columns) as i32 + self.min.0,
            (index / self.columns) as i32 + self.min.1,
        )
    }

    fn pos(&self, (i, j): (i32, i32)) -> Pos2 {
        self.origin + vec2(i as f32 * self.cell.x, j as f32 * self.cell.y)
    }

    /// A* from `(0, 0)` to `goal` over (cell, direction) states, with turn costs.
    /// Starts heading right (out of the output); arriving at the goal heading
    /// anywhere but right costs a turn. Returns the cells along the path, or `None`
    /// if there is none or the search expanded [`MAX_EXPANDED`] states without
    /// reaching the goal.
    fn search(&self, buffers: &mut SearchBuffers) -> Option<Vec<(i32, i32)>> {
        let start = self.index((0, 0))?;
        let goal = self.index(self.goal)?;
        let state = |index: usize, direction: usize| index * 4 + direction;
        let heuristic = |cell: (i32, i32), direction: usize| {
            let (di, dj) = (self.goal.0 - cell.0, self.goal.1 - cell.1);
            (di.unsigned_abs() + dj.unsigned_abs()) * STEP_COST
                + min_turns(di, dj, direction) * TURN_COST
        };
        // A walled-in end would make A* explore the whole area before giving up,
        // on every layout change; bail out early instead.
        let has_way_out = |(i, j): (i32, i32)| {
            DIRECTIONS
                .iter()
                .filter_map(|(di, dj)| self.index((i + di, j + dj)))
                .any(|index| !self.is(index, BLOCKED))
        };
        if start != goal && !(has_way_out((0, 0)) && has_way_out(self.goal)) {
            return None;
        }

        buffers.start(self.flags.len());
        buffers.set(state(start, RIGHT), 0, NO_STATE);
        let h = heuristic((0, 0), RIGHT);
        buffers
            .open
            .push(Reverse((h, h, state(start, RIGHT) as u32)));

        let mut expanded = 0;
        while let Some(Reverse((estimate, h, current))) = buffers.open.pop() {
            let current = current as usize;
            let (index, direction) = (current / 4, current % 4);
            let current_cost = buffers.cost(current);
            // A state is pushed again whenever a cheaper way to it turns up; the
            // older, costlier entries are left in the heap and skipped here.
            if estimate > current_cost + h {
                continue;
            }
            if index == goal {
                return Some(self.cells_to(current, &buffers.states));
            }
            expanded += 1;
            if expanded > MAX_EXPANDED
                || (expanded == REACHABILITY_CHECK_AFTER && !self.connected(start, goal, buffers))
            {
                return None;
            }
            let cell = self.cell_of(index);
            for (next_direction, (di, dj)) in DIRECTIONS.iter().enumerate() {
                // Never reverse on the spot.
                if next_direction == direction ^ 1 {
                    continue;
                }
                let next_cell = (cell.0 + di, cell.1 + dj);
                let Some(next_index) = self.index(next_cell) else {
                    continue;
                };
                if self.is(next_index, BLOCKED) {
                    continue;
                }
                let mut step = STEP_COST;
                if next_direction != direction {
                    step += TURN_COST;
                }
                if next_index == goal && next_direction != RIGHT {
                    step += TURN_COST;
                }
                if self.is(next_index, NEAR) {
                    step += NEAR_COST;
                }
                // Stepping onto a sibling from off it crosses (or joins) it; moving
                // along one doesn't count.
                if self.is(next_index, WIRE)
                    || (self.is(next_index, SIBLING) && !self.is(index, SIBLING))
                {
                    step += WIRE_COST;
                }
                let next = state(next_index, next_direction);
                let next_cost = current_cost + step;
                if next_cost < buffers.cost(next) {
                    buffers.set(next, next_cost, current as u32);
                    let h = heuristic(next_cell, next_direction);
                    buffers.open.push(Reverse((next_cost + h, h, next as u32)));
                }
            }
        }
        None
    }

    /// Whether grid point `goal` can be reached from `start` through unblocked
    /// points at all. A breadth-first flood fill over points (not search states):
    /// much cheaper than letting A* find out.
    fn connected(&self, start: usize, goal: usize, buffers: &mut SearchBuffers) -> bool {
        let generation = buffers.generation;
        buffers.queue.clear();
        buffers.queue.push(start as u32);
        buffers.flooded[start] = generation;
        let mut next = 0;
        while let Some(&index) = buffers.queue.get(next) {
            next += 1;
            let index = index as usize;
            if index == goal {
                return true;
            }
            let (i, j) = self.cell_of(index);
            for (di, dj) in DIRECTIONS {
                if let Some(neighbor) = self.index((i + di, j + dj))
                    && buffers.flooded[neighbor] != generation
                    && !self.is(neighbor, BLOCKED)
                {
                    buffers.flooded[neighbor] = generation;
                    buffers.queue.push(neighbor as u32);
                }
            }
        }
        false
    }

    /// The cells from the start to `state`, following `came_from` back.
    fn cells_to(&self, mut state: usize, states: &[StateEntry]) -> Vec<(i32, i32)> {
        let mut cells = vec![self.cell_of(state / 4)];
        while states[state].came_from != NO_STATE {
            state = states[state].came_from as usize;
            cells.push(self.cell_of(state / 4));
        }
        cells.reverse();
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    fn node(x: f32, y: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(160.0, 90.0))
    }

    /// Whether any point along the polyline is strictly inside `rect`.
    fn path_hits(points: &[Pos2], rect: Rect) -> bool {
        points.windows(2).any(|segment| {
            (0..=20).any(|k| strictly_inside(segment[0].lerp(segment[1], k as f32 / 20.0), rect))
        })
    }

    /// Whether every segment is horizontal or vertical.
    fn orthogonal(points: &[Pos2]) -> bool {
        points
            .windows(2)
            .all(|s| s[0].x == s[1].x || s[0].y == s[1].y)
    }

    #[test]
    fn clear_connections_stay_curves() {
        let (a, b) = (node(0.0, 0.0), node(400.0, 0.0));
        let route = route(
            pos2(160.0, 45.0),
            pos2(400.0, 45.0),
            &[a, b],
            &[],
            &[],
            20.0,
        );
        assert!(matches!(route, Route::Curve(_)));
    }

    #[test]
    fn ends_too_far_apart_to_search_keep_the_curve() {
        // A node in the way forces a search, but the area is far too big for it
        // (and its cell counts would overflow `i32`).
        let (start, end) = (pos2(160.0, 45.0), pos2(1e12, 1e12));
        let blocker = Rect::from_center_size(start.lerp(end, 0.5), vec2(1e11, 1e11));

        let route = route(start, end, &[blocker], &[], &[], 20.0);

        assert!(matches!(route, Route::Curve(_)));
    }

    #[test]
    fn connections_go_around_a_node_in_the_way() {
        let (a, blocker, b) = (node(0.0, 0.0), node(250.0, 0.0), node(500.0, 0.0));
        let (start, end) = (pos2(160.0, 45.0), pos2(500.0, 45.0));

        let Route::Path(points) = route(start, end, &[a, blocker, b], &[], &[], 20.0) else {
            panic!("expected a routed path");
        };

        assert_eq!(points.first(), Some(&start));
        assert_eq!(points.last(), Some(&end));
        assert!(!path_hits(&points, blocker));
        assert!(orthogonal(&points));
        // Leaves rightwards and enters from the left.
        assert!(points[1].x > start.x && points[1].y == start.y);
        assert!(points[points.len() - 2].x < end.x);
    }

    #[test]
    fn connections_squeeze_out_between_closely_packed_nodes() {
        // Two nodes right of the output, `NODE_GAP` away, above and below its
        // row: the stub end is within `CLEARANCE` of them, but not walled in.
        let a = node(0.0, 0.0);
        let (above, below) = (node(170.0, -65.0), node(170.0, 55.0));
        let (blocker, b) = (node(400.0, 0.0), node(600.0, 0.0));
        let nodes = [a, above, below, blocker, b];

        let Route::Path(points) =
            route(pos2(160.0, 45.0), pos2(600.0, 45.0), &nodes, &[], &[], 20.0)
        else {
            panic!("expected a routed path");
        };

        assert!(nodes.iter().all(|rect| !path_hits(&points, *rect)));
        assert!(orthogonal(&points));
    }

    #[test]
    fn nearly_aligned_ends_stay_orthogonal() {
        // Socket rows 3 units apart: less than half a cell.
        let (a, blocker, b) = (node(0.0, 0.0), node(250.0, 0.0), node(500.0, 3.0));
        let (start, end) = (pos2(160.0, 45.0), pos2(500.0, 48.0));

        let Route::Path(points) = route(start, end, &[a, blocker, b], &[], &[], 20.0) else {
            panic!("expected a routed path");
        };

        assert_eq!(points.first(), Some(&start));
        assert_eq!(points.last(), Some(&end));
        assert!(orthogonal(&points), "{points:?}");
    }

    #[test]
    fn obstacles_outside_the_influence_area_do_not_change_the_route() {
        let (a, blocker, b) = (node(0.0, 0.0), node(250.0, 0.0), node(500.0, 0.0));
        let (start, end) = (pos2(160.0, 45.0), pos2(500.0, 45.0));
        let area = influence(start, end, 20.0);
        // Just outside the area on every side: still nodes the search grid could see
        // if the area were too tight.
        let far = [
            Rect::from_min_size(pos2(area.max.x + 1.0, 0.0), vec2(160.0, 90.0)),
            Rect::from_min_size(pos2(0.0, area.max.y + 1.0), vec2(160.0, 90.0)),
            Rect::from_min_size(pos2(0.0, area.min.y - 91.0), vec2(160.0, 90.0)),
        ];
        assert!(far.iter().all(|rect| !rect.intersects(area)));

        // A wire running right next to the area, as long as the area is tall.
        let far_wire = [
            pos2(area.max.x + 1.0, area.min.y),
            pos2(area.max.x + 1.0, area.max.y),
        ];
        let near_only = route(start, end, &[a, blocker, b], &[], &[], 20.0);
        let with_far = route(
            start,
            end,
            &[a, blocker, b, far[0], far[1], far[2]],
            &[&far_wire],
            &[],
            20.0,
        );

        assert!(matches!(near_only, Route::Path(_)));
        assert_eq!(near_only, with_far);
    }

    #[test]
    fn influence_area_contains_long_backward_curves() {
        // Output far right of the input: the handles (|dx| / 2) reach well past
        // both ends, beyond any fixed margin.
        let (start, end) = (pos2(10_000.0, 0.0), pos2(0.0, 0.0));
        let area = influence(start, end, 20.0);
        let curve = bezier(start, end, 20.0);

        assert!(sample(&curve).iter().all(|&p| area.contains(p)));
    }

    /// Routes `connections` (output, input, source ID) one after another the way the
    /// canvas does: each against the earlier routes and the stubs of all the others,
    /// with earlier routes from the same source as siblings.
    fn route_all(connections: &[(Pos2, Pos2, usize)], nodes: &[Rect]) -> Vec<Vec<Pos2>> {
        let mut routed: Vec<Vec<Pos2>> = Vec::new();
        for (index, &(start, end, source)) in connections.iter().enumerate() {
            let other_stubs: Vec<[Pos2; 2]> = connections
                .iter()
                .filter(|(.., other)| *other != source)
                .flat_map(|&(start, end, _)| stubs(start, end))
                .collect();
            let mut wires: Vec<&[Pos2]> = other_stubs.iter().map(|stub| &stub[..]).collect();
            let mut siblings: Vec<&[Pos2]> = Vec::new();
            for (points, (.., other)) in routed.iter().zip(connections) {
                if *other == source {
                    siblings.push(points);
                } else {
                    wires.push(points);
                }
            }
            let route = route(start, end, nodes, &wires, &siblings, 20.0);
            routed.push(route.polyline());
            assert_eq!(routed.len(), index + 1);
        }
        routed
    }

    #[test]
    fn connections_from_different_sources_avoid_crossing() {
        // Two sources stacked on the left, a gain node and a mixer with three inputs
        // to the right. Source 0 feeds the gain and the mixer's second input (around
        // the gain), source 1 the mixer's third input, the gain the first one.
        let source_0 = node(0.0, 0.0);
        let source_1 = node(0.0, 100.0);
        let gain = node(193.0, 0.0);
        let mixer = Rect::from_min_size(pos2(410.0, 0.0), vec2(160.0, 178.0));
        let connections = [
            (pos2(160.0, 49.0), pos2(193.0, 49.0), 0),
            (pos2(160.0, 49.0), pos2(410.0, 71.0), 0),
            (pos2(160.0, 149.0), pos2(410.0, 93.0), 1),
            (pos2(353.0, 49.0), pos2(410.0, 49.0), 2),
        ];

        let routes = route_all(&connections, &[source_0, source_1, gain, mixer]);

        for (i, a) in routes.iter().enumerate() {
            for (j, b) in routes.iter().enumerate().skip(i + 1) {
                if connections[i].2 != connections[j].2 {
                    assert_eq!(
                        crossings(a, &[b]),
                        0,
                        "routes {i} and {j} cross: {a:?} {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn connections_from_one_output_branch_off_instead_of_crossing() {
        // One output feeding a node far right, routed below a node in the way, and a
        // node below and close: its plain curve would cut across the first route.
        let source = node(0.0, 0.0);
        let blocker = node(260.0, 0.0);
        let (far, near) = (node(520.0, 110.0), node(270.0, 220.0));
        let start = pos2(160.0, 30.0);
        let connections = [
            (start, pos2(520.0, 140.0), 0),
            (start, pos2(270.0, 250.0), 0),
        ];
        let nodes = [source, blocker, far, near];
        let curve = sample(&bezier(connections[1].0, connections[1].1, 20.0));

        let routes = route_all(&connections, &nodes);

        assert!(
            crossings(&curve, &[&routes[0]]) > 0,
            "the layout must make the curve cross"
        );
        assert!(orthogonal(&routes[1]), "{:?}", routes[1]);
        assert_eq!(crossings(&routes[1], &[&routes[0]]), 0, "{routes:?}");
        // Branches off the first route's trunk: both leave along the same stretch.
        assert_eq!(routes[0][..2], routes[1][..2], "{routes:?}");
    }

    #[test]
    fn unavoidable_crossings_keep_the_curve() {
        // Two sources feeding a node's inputs in reverse order: the wires must cross
        // somewhere, so the second connection stays a plain curve.
        let (a, b) = (node(0.0, 0.0), node(0.0, 200.0));
        let target = Rect::from_min_size(pos2(400.0, 50.0), vec2(160.0, 178.0));
        let connections = [
            (pos2(160.0, 45.0), pos2(400.0, 111.0), 0),
            (pos2(160.0, 245.0), pos2(400.0, 89.0), 1),
        ];

        let routes = route_all(&connections, &[a, b, target]);

        let curve = sample(&bezier(connections[1].0, connections[1].1, 20.0));
        assert_eq!(routes[1], curve);
    }

    #[test]
    fn unreachable_inputs_fall_back_to_the_curve() {
        // The input end sits in a closed box with room inside: neither end is
        // walled in, but there's no way through, which the flood fill finds out.
        let (start, end) = (pos2(160.0, 45.0), pos2(700.0, 345.0));
        let boxed = [
            node(0.0, 0.0),
            Rect::from_min_max(pos2(560.0, 200.0), pos2(860.0, 220.0)),
            Rect::from_min_max(pos2(560.0, 480.0), pos2(860.0, 500.0)),
            Rect::from_min_max(pos2(560.0, 200.0), pos2(580.0, 500.0)),
            Rect::from_min_max(pos2(840.0, 200.0), pos2(860.0, 500.0)),
        ];

        let route = route(start, end, &boxed, &[], &[], 20.0);

        assert_eq!(route, Route::Curve(bezier(start, end, 20.0)));
    }

    #[test]
    fn still_clear_until_a_node_moves_into_the_route() {
        let (a, blocker, b) = (node(0.0, 0.0), node(250.0, 0.0), node(500.0, 0.0));
        let (start, end) = (pos2(160.0, 45.0), pos2(500.0, 45.0));
        let path = route(start, end, &[a, blocker, b], &[], &[], 20.0);
        assert!(matches!(path, Route::Path(_)));
        let curve = Route::Curve(bezier(start, end, 20.0));

        // The blocker moving off leaves the path fit to draw (just not the best).
        assert!(still_clear(&path, &[a, b]));
        assert!(still_clear(&curve, &[a, b]));
        // A node right across the middle blocks both.
        let across = Rect::from_min_max(pos2(300.0, -200.0), pos2(340.0, 300.0));
        assert!(!still_clear(&path, &[a, b, across]));
        assert!(!still_clear(&curve, &[a, b, across]));
    }

    #[test]
    fn min_turns_never_overestimates_simple_routes() {
        // Straight ahead: no turns. A row below, heading right: down, then right.
        assert_eq!(min_turns(5, 0, RIGHT), 0);
        assert_eq!(min_turns(5, 3, RIGHT), 2);
        // Heading down towards a goal below and ahead: one turn to the right.
        assert_eq!(min_turns(5, 3, 2), 1);
        assert_eq!(min_turns(0, 0, LEFT), 0);
    }

    #[test]
    fn backward_connections_loop_around_both_nodes() {
        // Input left of the output: the curve would cut through both nodes.
        let (a, b) = (node(400.0, 200.0), node(0.0, 200.0));
        let (start, end) = (pos2(560.0, 245.0), pos2(0.0, 245.0));

        let Route::Path(points) = route(start, end, &[a, b], &[], &[], 20.0) else {
            panic!("expected a routed path");
        };

        assert!(!path_hits(&points, a));
        assert!(!path_hits(&points, b));
    }
}

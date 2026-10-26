//! Editor state and the `eframe` application entry.
//!
//! [`VSignalNodesApp`] owns the graph being edited (a `core::Graph`: nodes and
//! connections, with its rules), the layout (positions, paint order) and the
//! transient UI state (socket selection, language, zoom). Rendering and input
//! handling live in the [`ui`] module, which reads this state directly but changes
//! it only through the methods here, so the editor rules can be unit-tested
//! without egui. Saving and opening documents is in [`persistence`].

use eframe::egui;
use std::collections::HashMap;
use v_signal_nodes_core::{
    AppError, Graph, GraphError, InputRef, Node, NodeId, OutputRef, ParamValue,
};
use v_signal_nodes_plugin_host::PluginHost;

mod commands;
mod localization;
#[cfg(target_os = "macos")]
mod native_menu;
mod persistence;
mod ui;

use localization::{Language, Text};
use ui::{NodeKind, NodeWidget, RouteCache, SocketRef};

/// Maximum number of nodes on the canvas.
const MAX_NODE_COUNT: usize = 256;

/// The node editor.
///
/// The graph rules (connections between existing sockets, one source per input,
/// no cycles, ids of removed nodes never reused) are kept by `core::Graph`.
/// Invariants the editor itself must keep on every add/remove path:
/// - `positions` and `z_order` hold exactly the nodes of `graph`, each once;
/// - `pending_source` and `drag` only refer to nodes that exist;
/// - `node_limit_reached` is set only by a failed [`add_node_at`](Self::add_node_at)
///   and cleared by the next successful add or any deletion;
/// - nodes never overlap, except the dragged node while the drag lasts: adds go
///   through [`add_node_at`](Self::add_node_at), drops through
///   [`end_drag`](Self::end_drag) and snapping through
///   [`snap_to_grid`](Self::snap_to_grid), all using `ui::placement`;
/// - node positions change only through those methods (and
///   [`drag_to`](Self::drag_to), [`cancel_drag`](Self::cancel_drag)), never by the
///   UI writing them itself.
pub struct VSignalNodesApp {
    /// The nodes (spec and parameter values) and their connections.
    graph: Graph,
    /// Top-left corner of every node in canvas coordinates; never negative.
    positions: HashMap<NodeId, egui::Pos2>,
    /// Every node in paint order: later nodes are drawn on top and win
    /// overlapping clicks.
    z_order: Vec<NodeId>,
    /// The last attempt to add a node failed because there are already
    /// [`MAX_NODE_COUNT`]. Shown as a warning in the debug panel.
    node_limit_reached: bool,
    /// Output socket picked by a primary click, waiting for an input to connect to.
    pending_source: Option<OutputRef>,
    /// Why the last connection attempt was refused (a cycle, or sockets that don't
    /// fit), if it was. Shown as a warning in the debug panel; cleared by the next
    /// successful connection, disconnection or source selection.
    connection_refused: Option<Text>,
    /// Node kind clicked in the palette this frame; the canvas adds it in the
    /// middle of the visible area and clears it. (Palette drags don't use this:
    /// they travel as an egui drag-and-drop payload.)
    requested_node: Option<NodeKind>,
    /// UI language; every visible string is resolved through it on each frame.
    language: Language,
    /// Canvas zoom chosen in the toolbar: 1.0 = one canvas unit per screen point.
    scale: f32,
    /// The node being dragged on the canvas, if any.
    drag: Option<NodeDrag>,
    /// Connection routes, derived from the graph and the layout and brought up to
    /// date by the canvas every frame. Never saved.
    routes: RouteCache,
    /// Loaded plugins, the source of every node type; nodes are built here (as
    /// `core::Node`s) before they go into `graph`.
    plugins: PluginHost,
    /// Every node type of `plugins`, in palette order (see [`palette`]).
    palette: Vec<NodeKind>,
    /// The system menu bar, which replaces the egui one on macOS. `None` in
    /// tests, or if installing it failed (then the egui menu bar is shown).
    #[cfg(target_os = "macos")]
    native_menu: Option<native_menu::NativeMenu>,
}

/// A node drag in progress, from [`begin_drag`](VSignalNodesApp::begin_drag) to
/// [`end_drag`](VSignalNodesApp::end_drag): the whole gesture is one move from
/// `from` to wherever the node ends up.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NodeDrag {
    node_id: NodeId,
    /// Where the node was when the drag began, canvas coordinates.
    from: egui::Pos2,
}

impl Default for VSignalNodesApp {
    fn default() -> Self {
        let plugins = builtin_plugins();
        let palette = palette(&plugins);
        Self {
            graph: Graph::new(),
            positions: HashMap::new(),
            z_order: Vec::new(),
            node_limit_reached: false,
            pending_source: None,
            connection_refused: None,
            requested_node: None,
            language: Language::English,
            scale: 1.0,
            drag: None,
            routes: RouteCache::default(),
            plugins,
            palette,
            #[cfg(target_os = "macos")]
            native_menu: None,
        }
    }
}

/// A plugin host with the plugins linked into the editor: `std` and the `stub`
/// placeholder. They are part of the build, so failing to load one is a bug.
fn builtin_plugins() -> PluginHost {
    let mut host = PluginHost::new();
    host.load(&v_signal_nodes_plugin_std::StdPlugin)
        .expect("the std plugin loads");
    host.load(&v_signal_nodes_plugin_stub::StubPlugin)
        .expect("the stub plugin loads");
    host
}

/// Every node type of `plugins`, in palette order: generators, then processors,
/// then outputs (the order a signal flows), by type id within each group.
fn palette(plugins: &PluginHost) -> Vec<NodeKind> {
    use v_signal_nodes_core::NodeKind as Role;
    let mut kinds: Vec<_> = plugins.specs().cloned().map(NodeKind::new).collect();
    // `specs` is already sorted by type id and the sort is stable.
    kinds.sort_by_key(|kind| match kind.spec().kind {
        Role::Generator => 0,
        Role::Processor => 1,
        Role::Output => 2,
    });
    kinds
}

impl VSignalNodesApp {
    /// Creates the editor and installs its theme (and on macOS the system menu
    /// bar) once for the app's lifetime.
    ///
    /// egui's own Cmd/Ctrl +/−/0 zoom (of the whole UI) is turned off: those keys
    /// zoom the canvas instead (`commands::Command::ZoomIn` and friends).
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let ctx = &creation_context.egui_ctx;
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        ui::install_theme(ctx);
        let app = Self::default();
        #[cfg(target_os = "macos")]
        let app = Self {
            native_menu: native_menu::NativeMenu::install(ctx, app.language).ok(),
            ..app
        };
        app
    }

    /// Whether the menus are in the system menu bar, so the egui one is hidden.
    #[cfg(target_os = "macos")]
    fn has_native_menu(&self) -> bool {
        self.native_menu.is_some()
    }

    /// Whether the menus are in the system menu bar, so the egui one is hidden.
    #[cfg(not(target_os = "macos"))]
    fn has_native_menu(&self) -> bool {
        false
    }

    /// Node `node_id` as placed on the canvas, if there is one.
    fn node(&self, node_id: NodeId) -> Option<NodeWidget<'_>> {
        let node = self.graph.node(node_id)?;
        Some(NodeWidget::new(node_id, node, self.positions[&node_id]))
    }

    /// Every node as placed on the canvas, in paint order.
    fn nodes(&self) -> impl Iterator<Item = NodeWidget<'_>> {
        self.z_order.iter().filter_map(|&id| self.node(id))
    }

    /// Moves node `node_id` to `pos` (canvas coordinates), if there is such a node.
    fn set_pos(&mut self, node_id: NodeId, pos: egui::Pos2) {
        if let Some(current) = self.positions.get_mut(&node_id) {
            *current = pos;
        }
    }

    /// Bounds of every node, in paint order.
    fn node_rects(&self) -> Vec<egui::Rect> {
        self.nodes().map(|node| node.rect()).collect()
    }

    /// The kind of node type `type_id` (the same spec as its palette entry), if a
    /// loaded plugin has it.
    fn kind_of(&self, type_id: &str) -> Option<NodeKind> {
        self.plugins.spec(type_id).cloned().map(NodeKind::new)
    }

    /// A new node of `kind`, with `params` (by parameter id) instead of their
    /// defaults.
    ///
    /// The node is built by `plugins` (a `core::Node` with its processor). The
    /// processor is dropped: there is no runtime to run it yet. Fails if the host
    /// can't build the node (an unknown type id), or a parameter is unknown or its
    /// value not accepted by the spec (`core::Node::set_param`).
    fn build_node<'a>(
        &self,
        kind: &NodeKind,
        params: impl IntoIterator<Item = (&'a str, ParamValue)>,
    ) -> Result<Node, AppError> {
        let mut node = self.plugins.build_node(&kind.spec().type_id)?.node;
        for (param_id, value) in params {
            node.set_param(param_id, value)?;
        }
        Ok(node)
    }

    /// Adds a node of `kind` with its top-left corner as close to `pos` (canvas
    /// coordinates) as possible without overlapping other nodes, and returns its
    /// id. If the host can't build the node (see [`build_node`](Self::build_node)),
    /// nothing is added.
    ///
    /// With [`MAX_NODE_COUNT`] nodes on the canvas, nothing is added and
    /// `node_limit_reached` is set instead.
    fn add_node_at(&mut self, kind: NodeKind, pos: egui::Pos2) -> Option<NodeId> {
        if self.graph.len() >= MAX_NODE_COUNT {
            self.node_limit_reached = true;
            return None;
        }
        let node = self.build_node(&kind, []).ok()?;
        self.node_limit_reached = false;
        let pos = ui::place_new_node(&self.node_rects(), ui::node_size(node.spec()), pos);
        let id = self.graph.add(node);
        self.positions.insert(id, pos);
        self.z_order.push(id);
        Some(id)
    }

    /// Removes node `node_id` together with every connection to or from it and
    /// drops a pending source or drag on it. Does nothing if there is no such node.
    fn remove_node(&mut self, node_id: NodeId) {
        if self.graph.remove(node_id).is_none() {
            return;
        }
        self.positions.remove(&node_id);
        self.z_order.retain(|&id| id != node_id);
        self.node_limit_reached = false;
        if self.dragged_node() == Some(node_id) {
            self.drag = None;
        }
        if self
            .pending_source
            .is_some_and(|source| source.node == node_id)
        {
            self.pending_source = None;
        }
    }

    /// Moves node `node_id` to the end of the paint order, so it is drawn above
    /// the others and wins overlapping clicks.
    fn raise_node(&mut self, node_id: NodeId) {
        if let Some(index) = self.z_order.iter().position(|&id| id == node_id) {
            let id = self.z_order.remove(index);
            self.z_order.push(id);
        }
    }

    /// Starts dragging node `node_id` (ending any other drag first). Does nothing
    /// if there is no such node.
    fn begin_drag(&mut self, node_id: NodeId) {
        self.end_drag();
        if let Some(&from) = self.positions.get(&node_id) {
            self.drag = Some(NodeDrag { node_id, from });
        }
    }

    /// The node being dragged, if any.
    fn dragged_node(&self) -> Option<NodeId> {
        self.drag.map(|drag| drag.node_id)
    }

    /// Moves the dragged node `node_id` to `target` (its new top-left corner,
    /// canvas coordinates), kept off negative coordinates. It may pass over other
    /// nodes: overlaps are only resolved when the drag ends.
    ///
    /// Does nothing unless `node_id` is the node being dragged, so the rest of a
    /// cancelled drag gesture is ignored.
    fn drag_to(&mut self, node_id: NodeId, target: egui::Pos2) {
        if self.dragged_node() == Some(node_id) {
            self.set_pos(node_id, target.max(egui::Pos2::ZERO));
        }
    }

    /// Where nodes would go if the drag in progress ended now (`placement::settle`):
    /// `(node id, new position)` for every node that moves, the dragged one
    /// included. Empty when there is no drag or nothing would move. The canvas
    /// paints this as the drop preview.
    fn drop_moves(&self) -> Vec<(NodeId, egui::Pos2)> {
        let Some(dragged) = self
            .dragged_node()
            .and_then(|dragged| self.z_order.iter().position(|&id| id == dragged))
        else {
            return Vec::new();
        };
        ui::settle(&self.node_rects(), dragged)
            .into_iter()
            .zip(&self.z_order)
            .filter(|(pos, id)| *pos != self.positions[id])
            .map(|(pos, &id)| (id, pos))
            .collect()
    }

    /// Finishes the drag in progress: the node lands in a free spot nearby or
    /// pushes its neighbors aside ([`drop_moves`](Self::drop_moves)). This is where
    /// the whole drag is known as one move (`from` → final positions), e.g. for
    /// an undo step.
    fn end_drag(&mut self) {
        for (node_id, pos) in self.drop_moves() {
            self.set_pos(node_id, pos);
        }
        self.drag = None;
    }

    /// Cancels the drag in progress and puts the node back where the drag began.
    /// (That spot is still free: no other node moves before the drag ends.)
    fn cancel_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.set_pos(drag.node_id, drag.from);
    }

    /// Snaps every node to the nearest point of a `step` grid that keeps clear of
    /// the others (`placement::snap`).
    ///
    /// A drag in progress (the shortcut can come mid-drag) is ended first: after
    /// the snap its start spot may be taken, so cancelling it could overlap nodes.
    fn snap_to_grid(&mut self, step: f32) {
        self.end_drag();
        let snapped = ui::snap(&self.node_rects(), step);
        for (&id, pos) in self.z_order.iter().zip(snapped) {
            self.positions.insert(id, pos);
        }
    }

    /// Selects output socket `source` as the pending source of the next connection.
    fn select_source(&mut self, source: OutputRef) {
        self.pending_source = Some(source);
        self.connection_refused = None;
    }

    /// Clears the pending source.
    fn clear_selection(&mut self) {
        self.pending_source = None;
    }

    /// Connects the pending source to input socket `to` (see
    /// [`connect`](Self::connect)) and clears the selection. Does nothing without a
    /// pending source.
    fn connect_pending(&mut self, to: InputRef) {
        if let Some(from) = self.pending_source.take() {
            self.connect(from, to);
        }
    }

    /// Connects output socket `from` to input socket `to`, replacing whatever fed
    /// `to` before (`core::Graph::connect`). Returns whether the connection was made.
    ///
    /// Refused (and `connection_refused` set to the reason) when the new edge would
    /// close a cycle, including connecting a node to itself (the graph must stay
    /// acyclic to be processed in order), or when the output can't feed the input
    /// (rate or channels, `core::NodeSocket::can_feed`). Also refused, without a
    /// warning, when a socket doesn't exist (the canvas only offers existing ones).
    fn connect(&mut self, from: OutputRef, to: InputRef) -> bool {
        match self.graph.connect(from, to) {
            Ok(_) => {
                self.connection_refused = None;
                true
            }
            Err(GraphError::Cycle) => {
                self.connection_refused = Some(Text::ConnectionCreatesCycle);
                false
            }
            Err(GraphError::Incompatible) => {
                self.connection_refused = Some(Text::ConnectionIncompatible);
                false
            }
            Err(GraphError::MissingNode(_) | GraphError::MissingSocket { .. }) => false,
        }
    }

    /// Removes the connection into input socket `to`, if any.
    fn disconnect_input(&mut self, to: InputRef) {
        self.graph.disconnect(to);
        self.connection_refused = None;
    }

    /// Removes every connection from output socket `from`.
    fn disconnect_output(&mut self, from: OutputRef) {
        self.graph.disconnect_output(from);
        self.connection_refused = None;
    }

    /// Every connection as `(output, input)`, sorted by output, then input.
    fn sorted_connections(&self) -> Vec<(OutputRef, InputRef)> {
        let mut connections: Vec<_> = self.graph.connections().collect();
        connections.sort_unstable();
        connections
    }
}

impl eframe::App for VSignalNodesApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui::render(self, ui);
        #[cfg(target_os = "macos")]
        if let Some(mut menu) = self.native_menu.take() {
            menu.sync(self);
            self.native_menu = Some(menu);
        }
    }

    /// Runs the commands picked in the system menu bar (macOS) before the frame's
    /// input is processed. Copy and Paste become the egui events their keys would
    /// produce, so they reach a focused text field or `ui::menu` like the keys do.
    #[cfg(target_os = "macos")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(menu) = &self.native_menu else {
            return;
        };
        for command in menu.take_commands() {
            match command {
                commands::Command::Copy => raw_input.events.push(egui::Event::Copy),
                // egui reads the clipboard and sends the text as `Event::Paste`.
                commands::Command::Paste => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                }
                command => self.execute(command),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_plugin_std::{GAIN, MIXER, SINK, SOURCE};
    use v_signal_nodes_plugin_stub::STUB_NODE;

    /// Adds a node of type `type_id` at `pos` (or the nearest free spot).
    fn add(app: &mut VSignalNodesApp, type_id: &str, pos: egui::Pos2) -> NodeId {
        app.add_node_at(app.kind_of(type_id).unwrap(), pos).unwrap()
    }

    /// An app with `count` mixers (three inputs, one output each), in a row.
    fn mixers(count: usize) -> (VSignalNodesApp, Vec<NodeId>) {
        let mut app = VSignalNodesApp::default();
        let ids = (0..count)
            .map(|i| add(&mut app, MIXER, egui::pos2(i as f32 * 300.0, 0.0)))
            .collect();
        (app, ids)
    }

    fn output(node_id: NodeId) -> OutputRef {
        OutputRef::new(node_id, 0)
    }

    fn input(node_id: NodeId, index: usize) -> InputRef {
        InputRef::new(node_id, index)
    }

    fn pos(app: &VSignalNodesApp, node_id: NodeId) -> egui::Pos2 {
        app.positions[&node_id]
    }

    /// Asserts that no two nodes of `app` overlap.
    pub(super) fn assert_apart(app: &VSignalNodesApp) {
        let nodes: Vec<_> = app.nodes().collect();
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                assert!(
                    !a.rect().intersects(b.rect()),
                    "nodes {} and {} overlap",
                    a.id,
                    b.id
                );
            }
        }
    }

    #[test]
    fn connect_replaces_existing_source() {
        let (mut app, ids) = mixers(3);

        assert!(app.connect(output(ids[0]), input(ids[2], 0)));
        assert!(app.connect(output(ids[1]), input(ids[2], 0)));

        assert_eq!(app.graph.connection_count(), 1);
        assert_eq!(app.graph.source(input(ids[2], 0)), Some(output(ids[1])));
    }

    #[test]
    fn connect_refuses_cycles() {
        let (mut app, ids) = mixers(3);

        let cycle = |app: &VSignalNodesApp| {
            matches!(app.connection_refused, Some(Text::ConnectionCreatesCycle))
        };

        assert!(!app.connect(output(ids[0]), input(ids[0], 0)));
        assert!(cycle(&app));

        assert!(app.connect(output(ids[0]), input(ids[1], 0)));
        assert!(app.connect(output(ids[1]), input(ids[2], 0)));
        assert!(!app.connect(output(ids[2]), input(ids[0], 0)));
        assert!(cycle(&app));
        assert_eq!(app.graph.connection_count(), 2);

        // A diamond is not a cycle.
        assert!(app.connect(output(ids[0]), input(ids[2], 1)));
        assert!(app.connection_refused.is_none());
    }

    #[test]
    fn connect_refuses_missing_sockets_without_the_cycle_warning() {
        let (mut app, ids) = mixers(2);

        assert!(!app.connect(OutputRef::new(ids[0], 1), input(ids[1], 0)));
        assert!(!app.connect(output(ids[0]), input(ids[1], 3)));
        app.remove_node(ids[1]);
        assert!(!app.connect(output(ids[0]), input(ids[1], 0)));

        assert!(app.connection_refused.is_none());
        assert_eq!(app.graph.connection_count(), 0);
    }

    #[test]
    fn connecting_the_pending_source_uses_and_clears_it() {
        let (app, ids) = mixers(2);
        let mut app = VSignalNodesApp {
            connection_refused: Some(Text::ConnectionCreatesCycle),
            ..app
        };

        app.connect_pending(input(ids[1], 0));
        assert_eq!(app.graph.connection_count(), 0, "no pending source");

        app.select_source(output(ids[0]));
        assert!(app.connection_refused.is_none());
        app.connect_pending(input(ids[1], 0));
        assert_eq!(app.graph.source(input(ids[1], 0)), Some(output(ids[0])));
        assert_eq!(app.pending_source, None);

        app.select_source(output(ids[1]));
        app.clear_selection();
        assert_eq!(app.pending_source, None);
    }

    #[test]
    fn disconnect_input_removes_only_that_input() {
        let (mut app, ids) = mixers(3);
        app.connect(output(ids[0]), input(ids[2], 0));
        app.connect(output(ids[1]), input(ids[2], 1));
        app.connection_refused = Some(Text::ConnectionCreatesCycle);

        app.disconnect_input(input(ids[2], 0));

        assert_eq!(app.graph.connection_count(), 1);
        assert_eq!(app.graph.source(input(ids[2], 1)), Some(output(ids[1])));
        assert!(app.connection_refused.is_none());
    }

    #[test]
    fn disconnect_output_removes_every_connection_from_it() {
        let (mut app, ids) = mixers(3);
        app.connect(output(ids[0]), input(ids[1], 0));
        app.connect(output(ids[0]), input(ids[2], 0));
        app.connect(output(ids[1]), input(ids[2], 1));
        app.connection_refused = Some(Text::ConnectionCreatesCycle);

        app.disconnect_output(output(ids[0]));

        assert_eq!(app.graph.connection_count(), 1);
        assert_eq!(app.graph.source(input(ids[2], 1)), Some(output(ids[1])));
        assert!(app.connection_refused.is_none());
    }

    #[test]
    fn connections_sort_by_source_then_target() {
        let (mut app, ids) = mixers(4);
        app.connect(output(ids[1]), input(ids[2], 0));
        app.connect(output(ids[0]), input(ids[3], 0));
        app.connect(output(ids[0]), input(ids[2], 1));

        assert_eq!(
            app.sorted_connections(),
            [
                (output(ids[0]), input(ids[2], 1)),
                (output(ids[0]), input(ids[3], 0)),
                (output(ids[1]), input(ids[2], 0)),
            ]
        );
    }

    #[test]
    fn remove_node_cleans_up_and_retires_its_id() {
        let (mut app, ids) = mixers(3);
        app.connect(output(ids[0]), input(ids[1], 0));
        app.connect(output(ids[1]), input(ids[2], 0));
        app.select_source(output(ids[1]));

        app.remove_node(ids[1]);

        assert_eq!(app.graph.len(), 2);
        assert_eq!(app.z_order, [ids[0], ids[2]]);
        assert!(!app.positions.contains_key(&ids[1]));
        assert_eq!(app.graph.connection_count(), 0);
        assert_eq!(app.pending_source, None);
        // The freed slot (its number) is reused, the id itself is not.
        let new = add(&mut app, SINK, egui::Pos2::ZERO);
        assert_eq!(new.index(), ids[1].index());
        assert_ne!(new, ids[1]);
        assert!(app.node(ids[1]).is_none());
    }

    #[test]
    fn nodes_added_at_the_same_spot_do_not_overlap() {
        let mut app = VSignalNodesApp::default();
        for _ in 0..5 {
            add(&mut app, MIXER, egui::pos2(100.0, 100.0));
        }

        assert_apart(&app);
    }

    /// Two gain nodes side by side, `NODE_GAP`-ish apart: ids 0 (left) and 1.
    fn two_nodes() -> (VSignalNodesApp, [NodeId; 2]) {
        let mut app = VSignalNodesApp::default();
        let left = add(&mut app, GAIN, egui::pos2(0.0, 0.0));
        let right = add(&mut app, GAIN, egui::pos2(300.0, 0.0));
        (app, [left, right])
    }

    #[test]
    fn dragging_passes_over_nodes_and_the_drop_lands_next_to_them() {
        let (mut app, [a, b]) = two_nodes();
        app.begin_drag(a);

        app.drag_to(a, egui::pos2(50.0, 20.0));
        assert_eq!(pos(&app, a), egui::pos2(50.0, 20.0));

        // Right on top of node b while dragging: nothing stops it.
        app.drag_to(a, egui::pos2(310.0, 20.0));
        assert_eq!(pos(&app, a), egui::pos2(310.0, 20.0));
        assert_eq!(pos(&app, b), egui::pos2(300.0, 0.0));
        // The preview names what the drop will move: a free spot is close to a lone
        // neighbor, so only the dragged node.
        let moves = app.drop_moves();
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].0, a);

        app.end_drag();
        assert_eq!(app.dragged_node(), None);
        assert_eq!(pos(&app, a), moves[0].1);
        assert_eq!(pos(&app, b), egui::pos2(300.0, 0.0));
        assert_apart(&app);
        // Outside a drag, moves are ignored and nothing would move.
        let dropped = pos(&app, a);
        app.drag_to(a, egui::pos2(0.0, 0.0));
        assert_eq!(pos(&app, a), dropped);
        assert!(app.drop_moves().is_empty());
    }

    #[test]
    fn a_drop_into_a_crowd_stays_and_pushes_the_others_aside() {
        // A column of three gain nodes, a little more than the gap apart, and a
        // fourth node dragged right onto the middle one: no free spot nearby.
        let mut app = VSignalNodesApp::default();
        let ids: Vec<_> = (0..4)
            .map(|_| add(&mut app, GAIN, egui::pos2(1000.0, 1000.0)))
            .collect();
        let step = app.node(ids[0]).unwrap().rect().height() + 12.0;
        for (i, &id) in ids[..3].iter().enumerate() {
            app.positions
                .insert(id, egui::pos2(100.0, 100.0 + i as f32 * step));
        }
        let middle = pos(&app, ids[1]);
        app.begin_drag(ids[3]);
        app.drag_to(ids[3], middle);

        app.end_drag();

        assert_eq!(pos(&app, ids[3]), middle, "the dropped node moved");
        assert_ne!(
            pos(&app, ids[1]),
            middle,
            "the node under it didn't make room"
        );
        assert_apart(&app);
    }

    #[test]
    fn a_drop_that_barely_overlaps_moves_only_the_dropped_node() {
        let (mut app, [a, b]) = two_nodes();
        let right_edge = app.node(a).unwrap().rect().right();
        app.begin_drag(b);

        // Just inside the gap next to node a.
        app.drag_to(b, egui::pos2(right_edge + 2.0, 0.0));
        app.end_drag();

        assert_eq!(pos(&app, a), egui::pos2(0.0, 0.0));
        // Nudged right just enough to keep the gap, on the same row.
        let gap = pos(&app, b).x - right_edge;
        assert!(gap > 2.0 && gap < 30.0, "gap {gap}");
        assert_eq!(pos(&app, b).y, 0.0);
        assert_apart(&app);
    }

    #[test]
    fn dragging_never_goes_negative() {
        let (mut app, [_, b]) = two_nodes();
        app.begin_drag(b);

        app.drag_to(b, egui::pos2(-50.0, -50.0));

        assert!(pos(&app, b).x >= 0.0 && pos(&app, b).y >= 0.0);
    }

    #[test]
    fn cancelling_a_drag_puts_the_node_back_and_ignores_the_rest() {
        let (mut app, [_, b]) = two_nodes();
        app.begin_drag(b);
        app.drag_to(b, egui::pos2(400.0, 300.0));

        app.cancel_drag();
        assert_eq!(pos(&app, b), egui::pos2(300.0, 0.0));
        assert_eq!(app.dragged_node(), None);

        app.drag_to(b, egui::pos2(500.0, 500.0));
        assert_eq!(pos(&app, b), egui::pos2(300.0, 0.0));
    }

    #[test]
    fn removing_the_dragged_node_ends_the_drag() {
        let (mut app, [a, _]) = two_nodes();
        app.begin_drag(a);

        app.remove_node(a);

        assert_eq!(app.drag, None);
        app.cancel_drag();
        assert_eq!(app.nodes().count(), 1);
    }

    #[test]
    fn snap_to_grid_aligns_nodes_without_overlap() {
        let (mut app, [a, b]) = two_nodes();
        app.positions.insert(a, egui::pos2(13.0, 38.0));
        app.positions.insert(b, egui::pos2(187.0, 61.0));

        app.snap_to_grid(50.0);

        for node in app.nodes() {
            assert_eq!(node.pos.x % 50.0, 0.0);
            assert_eq!(node.pos.y % 50.0, 0.0);
        }
        assert_apart(&app);
    }

    #[test]
    fn snapping_mid_drag_ends_the_drag() {
        let (mut app, [a, b]) = two_nodes();
        app.begin_drag(b);
        // Over `a`'s spot: after the snap, `b`'s start spot may be taken.
        app.drag_to(b, pos(&app, a));

        app.snap_to_grid(50.0);
        app.cancel_drag();

        assert_eq!(app.dragged_node(), None);
        assert_apart(&app);
    }

    #[test]
    fn palette_lists_every_plugin_node_in_signal_order() {
        let app = VSignalNodesApp::default();
        let type_ids: Vec<_> = app
            .palette
            .iter()
            .map(|kind| kind.spec().type_id.as_str())
            .collect();

        assert_eq!(type_ids, [SOURCE, GAIN, MIXER, STUB_NODE, SINK]);
    }

    #[test]
    fn nodes_are_built_by_core_and_placed() {
        let mut app = VSignalNodesApp::default();
        let stub = app.kind_of(STUB_NODE).unwrap();

        let id = app.add_node_at(stub.clone(), egui::Pos2::ZERO).unwrap();

        assert_eq!(app.z_order, [id]);
        assert_eq!(id.index(), 0);
        let node = app.node(id).unwrap();
        assert_eq!(node.node.type_id(), STUB_NODE);
        // The same size as the palette preview, which only has the spec.
        assert_eq!(node.rect().size(), ui::node_size(stub.spec()));
    }

    #[test]
    fn the_node_limit_is_kept() {
        let mut app = VSignalNodesApp::default();
        let gain = app.kind_of(GAIN).unwrap();
        for _ in 0..MAX_NODE_COUNT {
            let node = app.build_node(&gain, []).unwrap();
            let id = app.graph.add(node);
            app.positions.insert(id, egui::Pos2::ZERO);
            app.z_order.push(id);
        }

        assert_eq!(app.add_node_at(gain.clone(), egui::Pos2::ZERO), None);
        assert!(app.node_limit_reached);
        app.remove_node(app.z_order[0]);
        assert!(!app.node_limit_reached);
    }

    #[test]
    fn unknown_plugin_types_are_not_added() {
        let mut app = VSignalNodesApp::default();
        let unknown = v_signal_nodes_core::NodeSpec::new(
            "test.unknown",
            "Unknown",
            v_signal_nodes_core::NodeKind::Generator,
        );

        assert_eq!(
            app.add_node_at(NodeKind::new(unknown), egui::Pos2::ZERO),
            None
        );

        assert!(app.graph.is_empty());
        assert!(app.z_order.is_empty() && app.positions.is_empty());
    }

    #[test]
    fn raise_node_moves_it_to_the_top() {
        let (mut app, ids) = mixers(3);

        app.raise_node(ids[0]);

        assert_eq!(app.z_order, [ids[1], ids[2], ids[0]]);
        assert_eq!(app.graph.len(), 3);
    }
}

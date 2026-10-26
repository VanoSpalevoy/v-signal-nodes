//! Editor-side view of nodes and their geometry.
//!
//! This is what the canvas draws and edits. Every node type comes from a plugin:
//! a [`NodeKind`] wraps the plugin's `v-signal-nodes-core` [`NodeSpec`]. A placed
//! node lives in the editor's `core::Graph` (spec and parameter values) with its
//! position kept by the editor; a [`NodeWidget`] borrows both for one frame.
//!
//! Node text (title, socket and parameter names, choice options) isn't stored:
//! nodes keep where it comes from (socket index, parameter id, option index), and
//! the spec looks the label up by its [`TextKey`] at paint time
//! ([`NodeKind::label`], [`NodeWidget::socket_label`], [`NodeWidget::param_label`]),
//! so any plugin can ship its own translations and existing nodes follow language
//! changes. The editor only adds the "on"/"off" of toggles.
//!
//! All positions here are **canvas coordinates**: `(0, 0)` is the top-left of the
//! scrollable canvas content, independent of scrolling and window layout. The
//! canvas converts them to screen coordinates when drawing.
//!
//! # Node layout
//!
//! ```text
//!   pos
//!    ┌──────────────────────────────┐ ─┬─
//!    │ Gain #3                  ✕   │  │ CONTENT_TOP: tinted header (title, delete)
//!    │                              │ ─┴─
//!    ● Audio in         Audio out   ●    socket row 0, ROW_HEIGHT tall
//!    │          Gain: 1.0           │    parameter row 0
//!    └──────────────────────────────┘
//!    ◄───────── NODE_WIDTH ─────────►
//! ```
//!
//! Socket rows come first: row `i` holds input `i` on the left edge and output `i`
//! on the right edge. Parameter rows follow below, centered, one per parameter, so
//! parameter text never overlaps socket labels. The node is tall enough for all
//! rows, but at least [`MIN_NODE_HEIGHT`]; the size depends only on the spec
//! ([`node_size`]).

use std::sync::Arc;

use eframe::egui;
use v_signal_nodes_core::{
    InputRef, Node, NodeId, NodeSpec, OutputRef, ParamValue, SocketDirection, TextKey,
};

use crate::editor::{Language, Text};

/// Width of every node.
const NODE_WIDTH: f32 = 160.0;
/// Height of a node with a single row.
const MIN_NODE_HEIGHT: f32 = 90.0;
/// Height of one socket/parameter row.
const ROW_HEIGHT: f32 = 22.0;
/// Height of the header above the first row.
const CONTENT_TOP: f32 = 38.0;

/// Type of a node: the spec of a node type from a loaded plugin. Decides its
/// sockets, parameters and color. Cheap to clone: the spec is the plugin host's,
/// shared with the palette and every node of this type.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::editor) struct NodeKind(Arc<NodeSpec>);

impl NodeKind {
    pub(in crate::editor) fn new(spec: impl Into<Arc<NodeSpec>>) -> Self {
        Self(spec.into())
    }

    /// The plugin's description of this node type.
    pub(in crate::editor) fn spec(&self) -> &NodeSpec {
        &self.0
    }

    /// Display name in `language`, used for the palette button and the node title.
    pub(super) fn label(&self, language: Language) -> &str {
        self.0.label(language.code(), TextKey::Name)
    }
}

/// Number of socket rows of a node of `spec`: the longer of the input and output
/// columns.
fn socket_rows(spec: &NodeSpec) -> usize {
    spec.inputs.len().max(spec.outputs.len())
}

/// Size of a node of `spec`: [`NODE_WIDTH`] wide, tall enough for the socket rows
/// plus one row per parameter.
///
/// The single source of node size: the node itself, the drop preview and the
/// canvas extent all use it.
pub(in crate::editor) fn node_size(spec: &NodeSpec) -> egui::Vec2 {
    let row_count = (socket_rows(spec) + spec.params.len()).max(1);
    egui::vec2(
        NODE_WIDTH,
        MIN_NODE_HEIGHT.max(CONTENT_TOP + ROW_HEIGHT * row_count as f32 + 8.0),
    )
}

/// Any socket of a node, input or output: what the canvas hit-tests, hovers and
/// paints. Graph edits take the typed [`InputRef`] / [`OutputRef`] inside.
///
/// Inputs are on the left edge, outputs on the right. Ordered by side (inputs
/// first), then node and index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::editor) enum SocketRef {
    Input(InputRef),
    Output(OutputRef),
}

impl SocketRef {
    /// The node the socket belongs to.
    pub(in crate::editor) fn node(self) -> NodeId {
        match self {
            Self::Input(input) => input.node,
            Self::Output(output) => output.node,
        }
    }

    pub(in crate::editor) fn side(self) -> SocketDirection {
        match self {
            Self::Input(_) => SocketDirection::Input,
            Self::Output(_) => SocketDirection::Output,
        }
    }

    /// 0-based row among the sockets on that side; shown 1-based in the UI.
    pub(in crate::editor) fn index(self) -> usize {
        match self {
            Self::Input(input) => input.index,
            Self::Output(output) => output.index,
        }
    }
}

/// A node placed on the canvas, as seen for one frame: its id, the graph node
/// (spec and parameter values) and its position.
#[derive(Clone, Copy, Debug)]
pub(in crate::editor) struct NodeWidget<'a> {
    /// The node's id in the graph; its index is shown in the node title.
    pub(in crate::editor) id: NodeId,
    pub(in crate::editor) node: &'a Node,
    /// Top-left corner in canvas coordinates; never negative (`placement` keeps it so).
    pub(in crate::editor) pos: egui::Pos2,
}

impl<'a> NodeWidget<'a> {
    pub(in crate::editor) fn new(id: NodeId, node: &'a Node, pos: egui::Pos2) -> Self {
        Self { id, node, pos }
    }

    /// The node's type.
    pub(in crate::editor) fn spec(&self) -> &'a NodeSpec {
        self.node.spec()
    }

    /// The node's bounds in canvas coordinates.
    pub(in crate::editor) fn rect(&self) -> egui::Rect {
        egui::Rect::from_min_size(self.pos, node_size(self.spec()))
    }

    /// `key` in `language`, translated by this node's spec (English when it has
    /// no translation).
    fn text(&self, key: TextKey<&str>, language: Language) -> &'a str {
        self.spec().label(language.code(), key)
    }

    /// Display name of the node's type in `language`, for the title.
    pub(super) fn title(&self, language: Language) -> &'a str {
        self.text(TextKey::Name, language)
    }

    /// Name of `socket` in `language`.
    pub(super) fn socket_label(&self, socket: SocketRef, language: Language) -> &'a str {
        self.text(TextKey::socket(socket.side(), socket.index()), language)
    }

    /// Number of parameters, one row each.
    pub(super) fn param_count(&self) -> usize {
        self.node.params().len()
    }

    /// `Name: value` of parameter `index` in `language`, for example "Gain: 1.0"
    /// or "Режим: стерео".
    pub(super) fn param_label(&self, index: usize, language: Language) -> String {
        let id = self.spec().params[index].id.as_str();
        let name = self.text(TextKey::Param(id), language);
        let value = match self.node.params()[index] {
            ParamValue::Choice(option) => self.text(TextKey::Option(id, option), language),
            ParamValue::Number(value) => return format!("{name}: {value:.1}"),
            ParamValue::Toggle(on) => language.text(if on { Text::On } else { Text::Off }),
        };
        format!("{name}: {value}")
    }

    /// Current parameter values by parameter id, in spec order.
    pub(in crate::editor) fn param_values(&self) -> impl Iterator<Item = (&'a str, ParamValue)> {
        let ids = self.spec().params.iter().map(|param| param.id.as_str());
        ids.zip(self.node.params().iter().copied())
    }

    /// Every socket of the node: inputs top to bottom, then outputs.
    pub(in crate::editor) fn sockets(&self) -> impl Iterator<Item = SocketRef> + use<> {
        let id = self.id;
        let count = |side| self.node.sockets(side).len();
        let inputs =
            (0..count(SocketDirection::Input)).map(move |i| SocketRef::Input(InputRef::new(id, i)));
        let outputs = (0..count(SocketDirection::Output))
            .map(move |i| SocketRef::Output(OutputRef::new(id, i)));
        inputs.chain(outputs)
    }

    /// Center of a socket in canvas coordinates: on the left (input) or right
    /// (output) edge, vertically centered in row `index`.
    ///
    /// `index` must be a valid socket on `side` (checked in debug builds).
    pub(super) fn socket_pos(&self, side: SocketDirection, index: usize) -> egui::Pos2 {
        debug_assert!(
            index < self.node.sockets(side).len(),
            "node {} has no {side:?} socket {index}",
            self.id,
        );
        egui::pos2(
            match side {
                SocketDirection::Input => self.pos.x,
                SocketDirection::Output => self.pos.x + NODE_WIDTH,
            },
            self.pos.y + CONTENT_TOP + ROW_HEIGHT * (index as f32 + 0.5),
        )
    }

    /// Center of parameter `index`'s text in canvas coordinates: horizontally
    /// centered, in the `index`-th row below the socket rows.
    pub(super) fn param_pos(&self, index: usize) -> egui::Pos2 {
        let row = socket_rows(self.spec()) + index;
        egui::pos2(
            self.pos.x + NODE_WIDTH * 0.5,
            self.pos.y + CONTENT_TOP + ROW_HEIGHT * (row as f32 + 0.5),
        )
    }
}

/// The kind of standard node `type_id` (e.g. `v_signal_nodes_plugin_std::GAIN`),
/// for tests that need real nodes without an app.
#[cfg(test)]
pub(in crate::editor) fn std_kind(type_id: &str) -> NodeKind {
    let mut host = v_signal_nodes_plugin_host::PluginHost::new();
    host.load(&v_signal_nodes_plugin_std::StdPlugin).unwrap();
    NodeKind::new(Arc::clone(host.spec(type_id).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::{Graph, NodeSocket, ParamSpec};
    use v_signal_nodes_plugin_std::{GAIN, MIXER};

    /// A graph holding one node of `spec`, and its id.
    fn one_node(spec: Arc<NodeSpec>) -> (Graph, NodeId) {
        let mut graph = Graph::new();
        let id = graph.add(Node::new(spec));
        (graph, id)
    }

    fn std_spec(type_id: &str) -> Arc<NodeSpec> {
        std_kind(type_id).0
    }

    #[test]
    fn node_geometry_expands_for_custom_sockets_and_content() {
        let (graph, id) = one_node(std_spec(MIXER));
        let node = NodeWidget::new(id, graph.node(id).unwrap(), egui::Pos2::ZERO);

        assert!(node.rect().height() > MIN_NODE_HEIGHT);
        assert_eq!(node.rect().size(), node_size(node.spec()));
        assert_eq!(node.sockets().count(), 4);
        assert_eq!(
            node.sockets().last(),
            Some(SocketRef::Output(OutputRef::new(id, 0)))
        );
        assert_ne!(
            node.socket_pos(SocketDirection::Input, 0),
            node.socket_pos(SocketDirection::Input, 2),
        );
    }

    #[test]
    fn params_sit_below_sockets_inside_node() {
        let (graph, id) = one_node(std_spec(MIXER));
        let node = NodeWidget::new(id, graph.node(id).unwrap(), egui::Pos2::ZERO);
        let last_socket = node.socket_pos(SocketDirection::Input, 2);
        let first_param = node.param_pos(0);
        let last_param = node.param_pos(2);

        assert!(first_param.y >= last_socket.y + ROW_HEIGHT);
        assert!(node.rect().contains(last_param));
    }

    #[test]
    fn node_param_label_follows_language() {
        let (graph, id) = one_node(std_spec(GAIN));
        let node = NodeWidget::new(id, graph.node(id).unwrap(), egui::Pos2::ZERO);

        assert_eq!(node.param_label(0, Language::English), "Gain: 1.0");
        assert_eq!(node.param_label(0, Language::Russian), "Усиление: 1.0");
    }

    #[test]
    fn labels_are_translated_by_the_spec() {
        let spec = NodeSpec::new(
            "test.gain",
            "Gain",
            v_signal_nodes_core::NodeKind::Processor,
        )
        .input(NodeSocket::audio_in("in"))
        .output(NodeSocket::audio_out("out"))
        .param(ParamSpec::choice("mode", "Mode", ["soft", "hard"], 1))
        .param(ParamSpec::toggle("on", "Enabled", true))
        .translate(
            "ru",
            [
                (TextKey::Name, "Усиление"),
                (TextKey::param("mode"), "Режим"),
                (TextKey::option("mode", 1), "жёсткий"),
            ],
        );
        let kind = NodeKind::new(spec);
        let (graph, id) = one_node(kind.0.clone());
        let node = NodeWidget::new(id, graph.node(id).unwrap(), egui::Pos2::ZERO);

        assert_eq!(kind.label(Language::Russian), "Усиление");
        assert_eq!(node.title(Language::Russian), "Усиление");
        assert_eq!(
            node.socket_label(SocketRef::Input(InputRef::new(id, 0)), Language::Russian),
            "in"
        );
        let labels = |language| {
            (0..node.param_count())
                .map(|index| node.param_label(index, language))
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(Language::English), ["Mode: hard", "Enabled: on"]);
        assert_eq!(
            labels(Language::Russian),
            ["Режим: жёсткий", "Enabled: вкл"]
        );
    }
}

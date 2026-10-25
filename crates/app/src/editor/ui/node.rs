//! Editor-side node model and geometry.
//!
//! This is what the canvas draws and edits. Every node type comes from a plugin:
//! a [`NodeKind`] wraps the plugin's `v-signal-nodes-core` [`NodeSpec`], and a
//! placed node is first built as a `core::Node` by the plugin host, then turned
//! into a [`NodeConfig`] with [`NodeConfig::from_node`].
//!
//! Node text (title, socket and parameter names, choice options) isn't stored:
//! nodes keep where it comes from (socket index, parameter id, option index), and
//! the spec looks the label up by its [`TextKey`] at paint time
//! ([`NodeKind::label`], [`NodeConfig::socket_label`], [`NodeConfig::param_label`]),
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
//! rows, but at least [`MIN_NODE_HEIGHT`].

use std::sync::Arc;

use eframe::egui;
use v_signal_nodes_core::{Node, NodeSpec, ParamSpec, ParamValue, TextKey};

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
/// sockets, parameters and color. Cheap to clone (the spec is shared).
#[derive(Clone, Debug, PartialEq)]
pub(in crate::editor) struct NodeKind(Arc<NodeSpec>);

impl NodeKind {
    pub(in crate::editor) fn new(spec: NodeSpec) -> Self {
        Self(Arc::new(spec))
    }

    /// The plugin's description of this node type.
    pub(in crate::editor) fn spec(&self) -> &NodeSpec {
        &self.0
    }

    /// Display name in `language`, used for the palette button and the node title.
    pub(super) fn label(&self, language: Language) -> &str {
        self.0.label(language.code(), &TextKey::Name)
    }
}

/// Side of a node a socket is on. Inputs are on the left edge, outputs on the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::editor) enum SocketKind {
    Input,
    Output,
}

impl SocketKind {
    /// Display name ("Input"/"Output"), used in the debug panel.
    pub(super) fn label(self, language: Language) -> &'static str {
        match self {
            Self::Input => language.text(Text::Input),
            Self::Output => language.text(Text::Output),
        }
    }
}

/// Address of one socket: node, side and row.
///
/// Used as both key and value of `VSignalNodesApp::connections`. Ordered by node,
/// side, then index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::editor) struct SocketRef {
    /// ID of the node ([`NodeWidget::id`]).
    pub(in crate::editor) node_id: usize,
    pub(in crate::editor) side: SocketKind,
    /// 0-based row among the sockets on that side; shown 1-based in the UI.
    pub(in crate::editor) index: usize,
}

impl SocketRef {
    /// Input socket `index` of node `node_id`.
    pub(in crate::editor) fn input(node_id: usize, index: usize) -> Self {
        Self {
            node_id,
            side: SocketKind::Input,
            index,
        }
    }

    /// Output socket `index` of node `node_id`.
    pub(in crate::editor) fn output(node_id: usize, index: usize) -> Self {
        Self {
            node_id,
            side: SocketKind::Output,
            index,
        }
    }
}

/// A named node parameter, shown as `Name: value` in its own row below the sockets.
/// Display only for now: parameters can't be edited yet.
#[derive(Clone, Debug)]
pub(in crate::editor) struct NodeParam {
    /// The parameter's id in the spec; its name (and a choice's option labels)
    /// come from the spec.
    id: String,
    value: ParamValue,
}

/// What a node contains: its kind, socket counts and parameters.
///
/// Text is looked up in the spec when shown, so nodes follow language changes.
#[derive(Clone, Debug)]
pub(in crate::editor) struct NodeConfig {
    pub(super) kind: NodeKind,
    /// Number of input sockets.
    inputs: usize,
    /// Number of output sockets.
    outputs: usize,
    /// Parameters, top to bottom, in spec order.
    pub(super) params: Vec<NodeParam>,
}

impl NodeConfig {
    /// A new node of `kind` as its spec describes it, with every parameter at its
    /// default. Placed nodes use [`from_node`](Self::from_node) instead; this is
    /// for sizes and previews before the node exists.
    pub(in crate::editor) fn for_kind(kind: &NodeKind) -> Self {
        let spec = kind.spec();
        let defaults: Vec<_> = spec.params.iter().map(ParamSpec::default_value).collect();
        Self::from_parts(
            kind.clone(),
            spec.inputs.len(),
            spec.outputs.len(),
            &defaults,
        )
    }

    /// The config of `node`, built by `core` from `kind`'s spec: its sockets and
    /// current parameter values.
    pub(in crate::editor) fn from_node(kind: NodeKind, node: &Node) -> Self {
        debug_assert_eq!(kind.spec().type_id, node.type_id);
        Self::from_parts(kind, node.inputs.len(), node.outputs.len(), &node.params)
    }

    fn from_parts(kind: NodeKind, inputs: usize, outputs: usize, values: &[ParamValue]) -> Self {
        let params = kind
            .spec()
            .params
            .iter()
            .zip(values)
            .map(|(param, &value)| NodeParam {
                id: param.id.clone(),
                value,
            })
            .collect();
        Self {
            kind,
            inputs,
            outputs,
            params,
        }
    }

    /// `key` in `language`, translated by this node's spec (English when it has
    /// no translation).
    fn text(&self, key: &TextKey, language: Language) -> &str {
        self.kind.spec().label(language.code(), key)
    }

    /// Name of socket `index` on `side` in `language`.
    pub(super) fn socket_label(&self, side: SocketKind, index: usize, language: Language) -> &str {
        let key = match side {
            SocketKind::Input => TextKey::Input(index),
            SocketKind::Output => TextKey::Output(index),
        };
        self.text(&key, language)
    }

    /// `Name: value` of `param` in `language`, for example "Gain: 1.0" or
    /// "Режим: стерео".
    pub(super) fn param_label(&self, param: &NodeParam, language: Language) -> String {
        let name = self.text(&TextKey::param(&param.id), language);
        let value = match param.value {
            ParamValue::Choice(index) => self.text(&TextKey::option(&param.id, index), language),
            ParamValue::Number(value) => return format!("{name}: {value:.1}"),
            ParamValue::Toggle(on) => language.text(if on { Text::On } else { Text::Off }),
        };
        format!("{name}: {value}")
    }

    /// Number of sockets on `side`.
    pub(in crate::editor) fn socket_count(&self, side: SocketKind) -> usize {
        match side {
            SocketKind::Input => self.inputs,
            SocketKind::Output => self.outputs,
        }
    }

    /// Number of socket rows: the longer of the input and output columns.
    fn socket_rows(&self) -> usize {
        self.inputs.max(self.outputs)
    }

    /// Size of a node with this config: [`NODE_WIDTH`] wide, tall enough for the
    /// socket rows plus one row per parameter.
    ///
    /// The single source of node size: the node itself, the drop preview and the
    /// canvas extent all use it.
    pub(super) fn size(&self) -> egui::Vec2 {
        let row_count = (self.socket_rows() + self.params.len()).max(1);
        egui::vec2(
            NODE_WIDTH,
            MIN_NODE_HEIGHT.max(CONTENT_TOP + ROW_HEIGHT * row_count as f32 + 8.0),
        )
    }
}

/// A node placed on the canvas.
#[derive(Clone, Debug)]
pub(in crate::editor) struct NodeWidget {
    /// Slot from `VSignalNodesApp::free_node_ids`; unique among live nodes and
    /// reused after the node is deleted. Shown in the node title.
    pub(in crate::editor) id: usize,
    pub(super) config: NodeConfig,
    /// Top-left corner in canvas coordinates; never negative (`placement` keeps it so).
    pub(in crate::editor) pos: egui::Pos2,
}

impl NodeWidget {
    pub(in crate::editor) fn new(id: usize, config: NodeConfig, pos: egui::Pos2) -> Self {
        Self { id, config, pos }
    }

    /// The node's bounds in canvas coordinates.
    pub(in crate::editor) fn rect(&self) -> egui::Rect {
        egui::Rect::from_min_size(self.pos, self.config.size())
    }

    /// The node's type.
    pub(in crate::editor) fn kind(&self) -> &NodeKind {
        &self.config.kind
    }

    /// Current parameter values by parameter id, in spec order.
    pub(in crate::editor) fn param_values(&self) -> impl Iterator<Item = (&str, ParamValue)> {
        self.config
            .params
            .iter()
            .map(|param| (param.id.as_str(), param.value))
    }

    /// Every socket of the node: inputs top to bottom, then outputs.
    pub(in crate::editor) fn sockets(&self) -> impl Iterator<Item = SocketRef> {
        let id = self.id;
        let side = |side: SocketKind| {
            (0..self.config.socket_count(side)).map(move |index| SocketRef {
                node_id: id,
                side,
                index,
            })
        };
        side(SocketKind::Input).chain(side(SocketKind::Output))
    }

    /// Whether the node has `socket` (its node ID is not checked).
    pub(in crate::editor) fn has_socket(&self, socket: SocketRef) -> bool {
        socket.index < self.config.socket_count(socket.side)
    }

    /// Center of a socket in canvas coordinates: on the left (input) or right
    /// (output) edge, vertically centered in row `index`.
    ///
    /// `index` must be a valid socket on `side` (checked in debug builds).
    pub(super) fn socket_pos(&self, side: SocketKind, index: usize) -> egui::Pos2 {
        debug_assert!(
            index < self.config.socket_count(side),
            "node {} has no {side:?} socket {index}",
            self.id,
        );
        egui::pos2(
            match side {
                SocketKind::Input => self.pos.x,
                SocketKind::Output => self.pos.x + NODE_WIDTH,
            },
            self.pos.y + CONTENT_TOP + ROW_HEIGHT * (index as f32 + 0.5),
        )
    }

    /// Center of parameter `index`'s text in canvas coordinates: horizontally
    /// centered, in the `index`-th row below the socket rows.
    pub(super) fn param_pos(&self, index: usize) -> egui::Pos2 {
        let row = self.config.socket_rows() + index;
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
    let spec = host.specs().find(|spec| spec.type_id == type_id).unwrap();
    NodeKind::new(spec.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::NodeSocket;
    use v_signal_nodes_plugin_std::{GAIN, MIXER};

    #[test]
    fn node_geometry_expands_for_custom_sockets_and_content() {
        let config = NodeConfig::for_kind(&std_kind(MIXER));
        let node = NodeWidget::new(0, config, egui::Pos2::ZERO);

        assert!(node.rect().height() > MIN_NODE_HEIGHT);
        assert_eq!(node.config.socket_count(SocketKind::Input), 3);
        assert_eq!(node.config.socket_count(SocketKind::Output), 1);
        assert_eq!(node.sockets().count(), 4);
        assert_eq!(node.sockets().last(), Some(SocketRef::output(0, 0)));
        assert_ne!(
            node.socket_pos(SocketKind::Input, 0),
            node.socket_pos(SocketKind::Input, 2),
        );
    }

    #[test]
    fn params_sit_below_sockets_inside_node() {
        let node = NodeWidget::new(0, NodeConfig::for_kind(&std_kind(MIXER)), egui::Pos2::ZERO);
        let last_socket = node.socket_pos(SocketKind::Input, 2);
        let first_param = node.param_pos(0);
        let last_param = node.param_pos(2);

        assert!(first_param.y >= last_socket.y + ROW_HEIGHT);
        assert!(node.rect().contains(last_param));
    }

    #[test]
    fn node_param_label_follows_language() {
        let config = NodeConfig::for_kind(&std_kind(GAIN));
        let param = &config.params[0];

        assert_eq!(config.param_label(param, Language::English), "Gain: 1.0");
        assert_eq!(
            config.param_label(param, Language::Russian),
            "Усиление: 1.0"
        );
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
        let config = NodeConfig::for_kind(&kind);

        assert_eq!(kind.label(Language::Russian), "Усиление");
        assert_eq!(
            config.socket_label(SocketKind::Input, 0, Language::Russian),
            "in"
        );
        let labels = |language| {
            config
                .params
                .iter()
                .map(|param| config.param_label(param, language))
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(Language::English), ["Mode: hard", "Enabled: on"]);
        assert_eq!(
            labels(Language::Russian),
            ["Режим: жёсткий", "Enabled: вкл"]
        );
    }
}

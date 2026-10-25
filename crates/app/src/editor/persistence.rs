//! Converting the edited graph to and from a saved [`Document`].
//!
//! The file format itself lives in [`crate::document`]; this module only maps the
//! editor's state onto it and checks every graph rule when a document is opened.

use std::collections::{HashMap, HashSet};

use eframe::egui;
use v_signal_nodes_core::AppError;

use super::{
    MAX_NODE_COUNT, NodeWidget, RouteCache, SocketRef, VSignalNodesApp, free_ids, reaches, ui,
};
use crate::document::{DocConnection, DocNode, Document};

impl VSignalNodesApp {
    /// The graph as a [`Document`] for saving: nodes in paint order with their
    /// type, position and parameters, every connection, and the plugins used.
    #[allow(dead_code, reason = "no Save command in the UI yet")]
    pub(crate) fn to_document(&self) -> Document {
        let used_plugins = self
            .nodes
            .iter()
            .filter_map(|node| self.plugins.plugin_of(&node.kind().spec().type_id))
            .map(str::to_owned)
            .collect();
        let nodes = self
            .nodes
            .iter()
            .map(|node| DocNode {
                id: node.id,
                type_id: node.kind().spec().type_id.clone(),
                pos: [node.pos.x, node.pos.y],
                params: node
                    .param_values()
                    .map(|(id, value)| (id.to_owned(), value.into()))
                    .collect(),
            })
            .collect();
        let connections = self
            .connections
            .iter()
            .map(|(to, from)| DocConnection {
                from_node: from.node_id,
                from_output: from.index,
                to_node: to.node_id,
                to_input: to.index,
            })
            .collect();
        Document::new(used_plugins, nodes, connections)
    }

    /// Replaces the graph with `document`'s, all or nothing: on error the editor
    /// is left unchanged.
    ///
    /// Fails if a used plugin isn't loaded, a node has an unknown type, a repeated
    /// or out-of-range ID ([`MAX_NODE_COUNT`]), an unknown parameter or a value its
    /// spec doesn't accept, or a connection refers to a missing node or socket,
    /// feeds an input twice or closes a cycle. Parameters left out keep their
    /// defaults. A node too close to an earlier one (a hand-edited file) is moved
    /// to the nearest free spot, as when added.
    ///
    /// Clears the transient state: selection, drag, warnings and routes.
    #[allow(dead_code, reason = "no Open command in the UI yet")]
    pub(crate) fn open_document(&mut self, document: &Document) -> Result<(), AppError> {
        let missing: Vec<_> = document
            .used_plugins
            .iter()
            .filter(|name| !self.plugins.plugins().contains(name))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(AppError::Plugin(format!(
                "the document needs plugins that aren't loaded: {}",
                missing.join(", ")
            )));
        }

        let mut nodes: Vec<NodeWidget> = Vec::with_capacity(document.nodes.len());
        for doc_node in &document.nodes {
            let id = doc_node.id;
            if id >= MAX_NODE_COUNT || nodes.iter().any(|node| node.id == id) {
                return Err(AppError::InvalidInput(format!(
                    "node ID {id} is out of range or repeated"
                )));
            }
            let kind = self.kind_of(&doc_node.type_id).ok_or_else(|| {
                AppError::InvalidInput(format!("node {id} has unknown type '{}'", doc_node.type_id))
            })?;
            let params = doc_node
                .params
                .iter()
                .map(|(param_id, &value)| (param_id.as_str(), value.into()));
            let config = self.build_config(kind, id, params)?;
            let [x, y] = doc_node.pos;
            let pos = ui::place_new_node(&nodes, &config, egui::pos2(x, y));
            nodes.push(NodeWidget::new(id, config, pos));
        }

        let mut connections = HashMap::new();
        for connection in &document.connections {
            let from = SocketRef::output(connection.from_node, connection.from_output);
            let to = SocketRef::input(connection.to_node, connection.to_input);
            let exists = |socket: SocketRef| {
                nodes
                    .iter()
                    .any(|node| node.id == socket.node_id && node.has_socket(socket))
            };
            if !exists(from) || !exists(to) {
                return Err(AppError::InvalidInput(format!(
                    "connection {connection:?} refers to a missing socket"
                )));
            }
            if connections.contains_key(&to) || reaches(&connections, to.node_id, from.node_id) {
                return Err(AppError::InvalidInput(format!(
                    "connection {connection:?} feeds an input twice or closes a cycle"
                )));
            }
            connections.insert(to, from);
        }

        let used: HashSet<_> = nodes.iter().map(|node| node.id).collect();
        self.free_node_ids = free_ids(&used);
        self.nodes = nodes;
        self.connections = connections;
        self.node_limit_reached = false;
        self.pending_source = None;
        self.connection_refused = false;
        self.requested_node = None;
        self.drag = None;
        self.routes = RouteCache::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocParamValue;
    use crate::editor::tests::assert_apart;
    use v_signal_nodes_core::ParamValue;
    use v_signal_nodes_plugin_std::{GAIN, MIXER, SINK, SOURCE};
    use v_signal_nodes_plugin_stub::STUB_NODE;

    /// An app with source 0 → gain 1 → mixer 2 (inputs 0 and 1) and a stub node 3.
    fn sample_graph() -> VSignalNodesApp {
        let mut app = VSignalNodesApp::default();
        for (type_id, x, y) in [
            (SOURCE, 0.0, 0.0),
            (GAIN, 250.0, 0.0),
            (MIXER, 500.0, 0.0),
            (STUB_NODE, 250.0, 300.0),
        ] {
            app.add_node_at(app.kind_of(type_id).unwrap(), egui::pos2(x, y));
        }
        assert!(app.connect(SocketRef::output(0, 0), SocketRef::input(1, 0)));
        assert!(app.connect(SocketRef::output(1, 0), SocketRef::input(2, 0)));
        assert!(app.connect(SocketRef::output(0, 0), SocketRef::input(2, 1)));
        app
    }

    /// The value of parameter `param_id` of node `node_id`.
    fn param(app: &VSignalNodesApp, node_id: usize, param_id: &str) -> ParamValue {
        app.node(node_id)
            .unwrap()
            .param_values()
            .find(|(id, _)| *id == param_id)
            .unwrap()
            .1
    }

    #[test]
    fn document_round_trip() {
        let mut app = sample_graph();
        app.raise_node(0);
        let document = app.to_document();

        assert_eq!(document.used_plugins, ["std", "stub"]);
        assert_eq!(document.nodes.len(), 4);
        assert_eq!(document.nodes[3].id, 0, "paint order is kept");
        assert_eq!(document.connections.len(), 3);

        let mut opened = VSignalNodesApp::default();
        opened.add_node_at(opened.kind_of(SINK).unwrap(), egui::pos2(900.0, 900.0));
        opened.select_source(SocketRef::output(0, 0));
        opened.open_document(&document).unwrap();

        assert_eq!(opened.to_document(), document);
        assert_eq!(opened.connections, app.connections);
        assert_eq!(opened.pending_source, None);
        assert_eq!(opened.free_node_ids.len(), MAX_NODE_COUNT - 4);
        opened.add_node_at(opened.kind_of(SINK).unwrap(), egui::Pos2::ZERO);
        assert_eq!(
            opened.nodes.last().unwrap().id,
            4,
            "IDs continue after the loaded ones"
        );
    }

    #[test]
    fn document_params_are_saved_and_checked() {
        let mut document = sample_graph().to_document();
        let gain = document.nodes.iter_mut().find(|node| node.id == 1).unwrap();
        assert_eq!(gain.params["gain"], DocParamValue::Number(1.0));
        gain.params
            .insert("gain".into(), DocParamValue::Number(0.25));
        let mut opened = VSignalNodesApp::default();

        opened.open_document(&document).unwrap();
        assert_eq!(param(&opened, 1, "gain"), ParamValue::Number(0.25));

        // Parameters left out get their defaults.
        for node in &mut document.nodes {
            node.params.clear();
        }
        opened.open_document(&document).unwrap();
        assert_eq!(param(&opened, 1, "gain"), ParamValue::Number(1.0));
    }

    #[test]
    fn open_document_refuses_broken_graphs_and_changes_nothing() {
        let good = sample_graph().to_document();
        let connection = |from_node, from_output, to_node, to_input| DocConnection {
            from_node,
            from_output,
            to_node,
            to_input,
        };
        let set_gain = |value: DocParamValue| {
            move |doc: &mut Document| {
                let gain = doc.nodes.iter_mut().find(|node| node.id == 1).unwrap();
                gain.params.insert("gain".into(), value);
            }
        };
        type Breakage<'a> = Box<dyn Fn(&mut Document) + 'a>;
        let broken: Vec<(&str, Breakage<'_>)> = vec![
            (
                "missing plugin",
                Box::new(|doc| doc.used_plugins.push("fx".into())),
            ),
            (
                "unknown type",
                Box::new(|doc| doc.nodes[0].type_id = "std.nope".into()),
            ),
            (
                "repeated id",
                Box::new(|doc| doc.nodes[1].id = doc.nodes[0].id),
            ),
            (
                "id out of range",
                Box::new(|doc| doc.nodes[0].id = MAX_NODE_COUNT),
            ),
            (
                "unknown param",
                Box::new(|doc| {
                    doc.nodes[0]
                        .params
                        .insert("nope".into(), DocParamValue::Toggle(true));
                }),
            ),
            (
                "param out of range",
                Box::new(set_gain(DocParamValue::Number(9.0))),
            ),
            (
                "param of the wrong kind",
                Box::new(set_gain(DocParamValue::Toggle(true))),
            ),
            (
                "missing node",
                Box::new(|doc| doc.connections.push(connection(0, 0, 9, 0))),
            ),
            (
                "missing output",
                Box::new(|doc| doc.connections.push(connection(0, 1, 3, 0))),
            ),
            (
                "missing input",
                Box::new(|doc| doc.connections.push(connection(0, 0, 2, 3))),
            ),
            (
                "input fed twice",
                Box::new(|doc| doc.connections.push(connection(3, 0, 1, 0))),
            ),
            (
                "cycle",
                Box::new(|doc| doc.connections.push(connection(1, 0, 1, 0))),
            ),
        ];

        for (case, break_it) in broken {
            let mut document = good.clone();
            break_it(&mut document);
            let mut opened = sample_graph();
            opened.select_source(SocketRef::output(3, 0));

            assert!(
                opened.open_document(&document).is_err(),
                "{case} was accepted"
            );
            assert_eq!(opened.to_document(), good, "{case} changed the graph");
            assert_eq!(
                opened.pending_source,
                Some(SocketRef::output(3, 0)),
                "{case}"
            );
        }
    }

    #[test]
    fn open_document_separates_overlapping_nodes() {
        let mut document = sample_graph().to_document();
        for node in &mut document.nodes {
            node.pos = [-50.0, 20.0];
        }
        let mut opened = VSignalNodesApp::default();

        opened.open_document(&document).unwrap();

        assert_apart(&opened);
        assert!(
            opened
                .nodes
                .iter()
                .all(|node| node.pos.x >= 0.0 && node.pos.y >= 0.0)
        );
    }
}

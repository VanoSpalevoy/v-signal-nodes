//! Converting the edited graph to and from a saved [`Document`].
//!
//! The file format itself lives in [`crate::document`]; this module only maps the
//! editor's state onto it and checks every graph rule when a document is opened.

use std::collections::{BTreeMap, HashMap};

use eframe::egui;
use v_signal_nodes_core::{Error, Graph, InputRef, OutputRef};

use super::{MAX_NODE_COUNT, RouteCache, VSignalNodesApp, ui};
use crate::document::{DocConnection, DocNode, Document, DocumentError};

impl VSignalNodesApp {
    /// The graph as a [`Document`] for saving: nodes in paint order with their
    /// type, position and parameters, every connection, and the plugins used.
    /// Node IDs are the node numbers (slot indices) shown on the canvas.
    #[allow(dead_code, reason = "no Save command in the UI yet")]
    pub(crate) fn to_document(&self) -> Document {
        let used_plugins = self
            .nodes()
            .filter_map(|node| self.plugins.plugin_of(node.node.type_id()))
            .map(|plugin| plugin.name.clone())
            .collect();
        let nodes = self
            .nodes()
            .map(|node| DocNode {
                id: node.id.index(),
                type_id: node.node.type_id().to_owned(),
                pos: [node.pos.x, node.pos.y],
                params: node
                    .param_values()
                    .map(|(id, value)| (id.to_owned(), value))
                    .collect(),
            })
            .collect();
        let connections = self
            .graph
            .connections()
            .map(|(from, to)| DocConnection {
                from_node: from.node.index(),
                from_output: from.index,
                to_node: to.node.index(),
                to_input: to.index,
            })
            .collect();
        Document::new(used_plugins, nodes, connections)
    }

    /// Replaces the graph with `document`'s, all or nothing: on error the editor
    /// is left unchanged.
    ///
    /// Fails if a used plugin isn't loaded, there are more than
    /// [`MAX_NODE_COUNT`] nodes, a node has an unknown type, a repeated ID, an
    /// unknown parameter or a value its spec doesn't accept, or a connection
    /// refers to a missing node or socket, feeds an input twice or closes a cycle
    /// (`core::Graph::connect`). Parameters left out keep their defaults. A node
    /// too close to an earlier one (a hand-edited file) is moved to the nearest
    /// free spot, as when added.
    ///
    /// Nodes get new ids in the order of their document IDs, so documents with
    /// IDs `0..n` (as saved) keep their node numbers; gaps are closed.
    ///
    /// Clears the transient state: selection, drag, warnings and routes.
    #[allow(dead_code, reason = "no Open command in the UI yet")]
    pub(crate) fn open_document(&mut self, document: &Document) -> Result<(), DocumentError> {
        let missing: Vec<_> = document
            .used_plugins
            .iter()
            .filter(|name| self.plugins.plugin(name).is_none())
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(DocumentError::MissingPlugins(missing));
        }

        if document.nodes.len() > MAX_NODE_COUNT {
            return Err(DocumentError::TooManyNodes {
                count: document.nodes.len(),
                limit: MAX_NODE_COUNT,
            });
        }
        let mut by_doc_id = BTreeMap::new();
        for doc_node in &document.nodes {
            if by_doc_id.insert(doc_node.id, doc_node).is_some() {
                return Err(DocumentError::RepeatedNode(doc_node.id));
            }
        }
        let mut graph = Graph::new();
        let mut ids = HashMap::new();
        for (&doc_id, doc_node) in &by_doc_id {
            let node_error = |error| DocumentError::Node {
                node: doc_id,
                error,
            };
            let kind = self
                .kind_of(&doc_node.type_id)
                .ok_or_else(|| node_error(Error::UnknownNodeType(doc_node.type_id.clone())))?;
            let params = doc_node
                .params
                .iter()
                .map(|(param_id, &value)| (param_id.as_str(), value));
            let node = self.build_node(&kind, params).map_err(node_error)?;
            ids.insert(doc_id, graph.add(node));
        }

        // Paint order is the document order; each node keeps clear of the earlier ones.
        let mut positions = HashMap::new();
        let mut z_order = Vec::with_capacity(document.nodes.len());
        let mut rects = Vec::with_capacity(document.nodes.len());
        for doc_node in &document.nodes {
            let id = ids[&doc_node.id];
            let size = ui::node_size(graph.node(id).expect("just added").spec());
            let [x, y] = doc_node.pos;
            let pos = ui::place_new_node(&rects, size, egui::pos2(x, y));
            rects.push(egui::Rect::from_min_size(pos, size));
            positions.insert(id, pos);
            z_order.push(id);
        }

        for connection in &document.connections {
            let node = |doc_id: usize| {
                ids.get(&doc_id)
                    .copied()
                    .ok_or(DocumentError::ConnectionToMissingNode(*connection))
            };
            let from = OutputRef::new(node(connection.from_node)?, connection.from_output);
            let to = InputRef::new(node(connection.to_node)?, connection.to_input);
            if graph.source(to).is_some() {
                return Err(DocumentError::InputFedTwice(*connection));
            }
            graph
                .connect(from, to)
                .map_err(|error| DocumentError::Connection {
                    connection: *connection,
                    error,
                })?;
        }

        self.graph = graph;
        self.mark_graph_replaced();
        self.positions = positions;
        self.z_order = z_order;
        self.node_limit_reached = false;
        self.pending_source = None;
        self.connection_refused = None;
        self.requested_node = None;
        self.drag = None;
        self.routes = RouteCache::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::tests::assert_apart;
    use v_signal_nodes_core::{NodeId, ParamValue};
    use v_signal_nodes_plugin_std::{GAIN, MIXER, SINK, SOURCE};
    use v_signal_nodes_plugin_stub_oscillograph::OSCILLOGRAPH;

    /// An app with source 0 → gain 1 → mixer 2 (inputs 0 and 1) and an oscillograph
    /// node 3, and their ids.
    fn sample_graph() -> (VSignalNodesApp, Vec<NodeId>) {
        let mut app = VSignalNodesApp::default();
        let ids: Vec<_> = [
            (SOURCE, 0.0, 0.0),
            (GAIN, 250.0, 0.0),
            (MIXER, 500.0, 0.0),
            (OSCILLOGRAPH, 250.0, 300.0),
        ]
        .into_iter()
        .map(|(type_id, x, y)| {
            app.add_node_at(app.kind_of(type_id).unwrap(), egui::pos2(x, y))
                .unwrap()
        })
        .collect();
        let out = |node: usize| OutputRef::new(ids[node], 0);
        let input = |node: usize, index| InputRef::new(ids[node], index);
        assert!(app.connect(out(0), input(1, 0)));
        assert!(app.connect(out(1), input(2, 0)));
        assert!(app.connect(out(0), input(2, 1)));
        (app, ids)
    }

    /// The value of parameter `param_id` of the node numbered `index`.
    fn param(app: &VSignalNodesApp, index: usize, param_id: &str) -> ParamValue {
        app.nodes()
            .find(|node| node.id.index() == index)
            .unwrap()
            .param_values()
            .find(|(id, _)| *id == param_id)
            .unwrap()
            .1
    }

    #[test]
    fn document_round_trip() {
        let (mut app, ids) = sample_graph();
        app.raise_node(ids[0]);
        let document = app.to_document();

        assert_eq!(document.used_plugins, ["std", "stub-oscillograph"]);
        assert_eq!(document.nodes.len(), 4);
        assert_eq!(document.nodes[3].id, 0, "paint order is kept");
        assert_eq!(document.connections.len(), 3);

        let mut opened = VSignalNodesApp::default();
        let sink = opened
            .add_node_at(opened.kind_of(SINK).unwrap(), egui::pos2(900.0, 900.0))
            .unwrap();
        opened.select_source(OutputRef::new(sink, 0));
        opened.open_document(&document).unwrap();

        assert_eq!(opened.to_document(), document);
        assert_eq!(
            opened.graph.connections().collect::<Vec<_>>(),
            app.graph.connections().collect::<Vec<_>>()
        );
        assert_eq!(opened.pending_source, None);
        assert_eq!(opened.graph.len(), 4);
        let added = opened
            .add_node_at(opened.kind_of(SINK).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        assert_eq!(added.index(), 4, "numbers continue after the loaded ones");
    }

    #[test]
    fn gaps_in_document_ids_are_closed() {
        let mut document = sample_graph().0.to_document();
        for node in &mut document.nodes {
            node.id *= 10;
        }
        for connection in &mut document.connections {
            connection.from_node *= 10;
            connection.to_node *= 10;
        }
        let mut opened = VSignalNodesApp::default();

        opened.open_document(&document).unwrap();

        assert_eq!(opened.to_document(), sample_graph().0.to_document());
    }

    #[test]
    fn document_params_are_saved_and_checked() {
        let mut document = sample_graph().0.to_document();
        let gain = document.nodes.iter_mut().find(|node| node.id == 1).unwrap();
        assert_eq!(gain.params["gain"], ParamValue::Number(1.0));
        gain.params.insert("gain".into(), ParamValue::Number(0.25));
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
        let good = sample_graph().0.to_document();
        let connection = |from_node, from_output, to_node, to_input| DocConnection {
            from_node,
            from_output,
            to_node,
            to_input,
        };
        let set_gain = |value: ParamValue| {
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
                "too many nodes",
                Box::new(|doc| {
                    let extra = doc.nodes[0].clone();
                    doc.nodes.extend((0..MAX_NODE_COUNT).map(|i| DocNode {
                        id: 100 + i,
                        ..extra.clone()
                    }));
                }),
            ),
            (
                "unknown param",
                Box::new(|doc| {
                    doc.nodes[0]
                        .params
                        .insert("nope".into(), ParamValue::Toggle(true));
                }),
            ),
            (
                "param out of range",
                Box::new(set_gain(ParamValue::Number(9.0))),
            ),
            (
                "param of the wrong kind",
                Box::new(set_gain(ParamValue::Toggle(true))),
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
            let (mut opened, ids) = sample_graph();
            let source = OutputRef::new(ids[3], 0);
            opened.select_source(source);

            assert!(
                opened.open_document(&document).is_err(),
                "{case} was accepted"
            );
            assert_eq!(opened.to_document(), good, "{case} changed the graph");
            assert_eq!(opened.pending_source, Some(source), "{case}");
        }
    }

    #[test]
    fn open_document_separates_overlapping_nodes() {
        let mut document = sample_graph().0.to_document();
        for node in &mut document.nodes {
            node.pos = [-50.0, 20.0];
        }
        let mut opened = VSignalNodesApp::default();

        opened.open_document(&document).unwrap();

        assert_apart(&opened);
        assert!(
            opened
                .nodes()
                .all(|node| node.pos.x >= 0.0 && node.pos.y >= 0.0)
        );
    }
}

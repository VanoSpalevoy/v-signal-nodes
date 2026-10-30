//! The node graph: nodes under generational ids and the connections between
//! their sockets.
//!
//! [`Graph`] holds the [`Node`]s (their specs and parameter values) and keeps
//! the rules every consumer relies on: a connection runs from an existing output
//! to an existing input that it can feed ([`NodeSocket::can_feed`]: same rate,
//! channels equal or one side mono), each input has at most one source, and there are no
//! cycles, so the nodes can always be processed in [`topo_order`](Graph::topo_order).
//! Layout (positions, paint order) is not part of the graph: that is the editor's.
//!
//! ```
//! use std::sync::Arc;
//! use v_signal_nodes_core::{Graph, InputRef, Node, NodeKind, NodeSocket, NodeSpec, OutputRef};
//!
//! let source = Arc::new(
//!     NodeSpec::new("my.source", "Source", NodeKind::Generator).output(NodeSocket::signal_out("Out")),
//! );
//! let sink = Arc::new(NodeSpec::new("my.sink", "Sink", NodeKind::Output).input(NodeSocket::signal_in("In")));
//!
//! let mut graph = Graph::new();
//! let a = graph.add(Node::new(source));
//! let b = graph.add(Node::new(sink));
//! graph.connect(OutputRef::new(a, 0), InputRef::new(b, 0))?;
//!
//! assert!(graph.connect(OutputRef::new(b, 0), InputRef::new(a, 0)).is_err(), "no such sockets");
//! assert_eq!(graph.topo_order(), [a, b]);
//! # Ok::<(), v_signal_nodes_core::GraphError>(())
//! ```

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

use crate::{Node, NodeSocket, SocketDirection};

/// Id of a node in a [`Graph`]: a slot index plus the slot's generation.
///
/// A removed node's slot is reused, but with the next generation, so an id kept
/// after its node was removed (in a message to the real-time thread, an undo step)
/// never refers to the node that took the slot. Ordered by index, then generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

impl NodeId {
    /// The slot index: small, unique among the graph's nodes, reused after a node
    /// is removed. What the editor shows as the node number.
    pub fn index(self) -> usize {
        self.index as usize
    }
}

impl fmt::Display for NodeId {
    /// The slot index only, as the editor shows it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.index)
    }
}

/// Input socket `index` (0-based, in spec order) of node `node`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputRef {
    pub node: NodeId,
    pub index: usize,
}

impl InputRef {
    pub fn new(node: NodeId, index: usize) -> Self {
        Self { node, index }
    }
}

/// Output socket `index` (0-based, in spec order) of node `node`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputRef {
    pub node: NodeId,
    pub index: usize,
}

impl OutputRef {
    pub fn new(node: NodeId, index: usize) -> Self {
        Self { node, index }
    }
}

/// Why [`Graph::connect`] refused a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// The node isn't in the graph (never was, or was removed).
    MissingNode(NodeId),
    /// The node has no such socket.
    MissingSocket {
        node: NodeId,
        direction: SocketDirection,
        index: usize,
    },
    /// The output can't feed the input: different rates, or channel counts that
    /// differ with neither side mono ([`NodeSocket::can_feed`]).
    Incompatible,
    /// The connection would close a cycle (a node feeding itself included).
    Cycle,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNode(node) => write!(f, "there is no node {node}"),
            Self::MissingSocket {
                node,
                direction,
                index,
            } => write!(f, "node {node} has no {direction:?} socket {index}"),
            Self::Incompatible => write!(f, "the sockets don't fit (rate or channels)"),
            Self::Cycle => write!(f, "the connection would close a cycle"),
        }
    }
}

impl std::error::Error for GraphError {}

/// One node slot: the node in it, if any, and its generation.
#[derive(Clone, Debug, PartialEq)]
struct Slot {
    generation: u32,
    node: Option<Node>,
}

/// Nodes and the connections between them; acyclic, each input fed at most once.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    slots: Vec<Slot>,
    /// Indices of the empty slots; the lowest is reused first.
    free: BTreeSet<u32>,
    /// Number of nodes (full slots).
    len: usize,
    /// Connections keyed by the input, so each input has at most one source.
    connections: BTreeMap<InputRef, OutputRef>,
    /// See [`revision`](Self::revision).
    revision: u64,
}

/// Two graphs are equal when they hold the same content, however they got there.
impl PartialEq for Graph {
    fn eq(&self, other: &Self) -> bool {
        self.slots == other.slots
            && self.free == other.free
            && self.len == other.len
            && self.connections == other.connections
    }
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grows with every change to this graph, so whoever derives something from it
    /// (a compiled plan) can tell it's out of date without comparing the content.
    /// [`node_mut`](Self::node_mut) counts as a change. Only comparable between
    /// states of one graph: a different graph may have the same number.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Adds `node` in the lowest free slot and returns its new id.
    pub fn add(&mut self, node: Node) -> NodeId {
        self.revision += 1;
        self.len += 1;
        if let Some(index) = self.free.pop_first() {
            let slot = &mut self.slots[index as usize];
            slot.node = Some(node);
            return NodeId {
                index,
                generation: slot.generation,
            };
        }
        let index = u32::try_from(self.slots.len()).expect("fewer than 2^32 nodes");
        self.slots.push(Slot {
            generation: 0,
            node: Some(node),
        });
        NodeId {
            index,
            generation: 0,
        }
    }

    /// Removes node `id` together with every connection to or from it. Returns the
    /// node, or `None` if there is no such node.
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        let slot = self.slots.get_mut(id.index())?;
        if slot.generation != id.generation {
            return None;
        }
        let node = slot.node.take()?;
        self.revision += 1;
        // A wrap after 2^32 removals from one slot is not a practical concern.
        slot.generation = slot.generation.wrapping_add(1);
        self.free.insert(id.index);
        self.len -= 1;
        self.connections
            .retain(|input, output| input.node != id && output.node != id);
        Some(node)
    }

    /// Whether node `id` is in the graph.
    pub fn contains(&self, id: NodeId) -> bool {
        self.node(id).is_some()
    }

    /// Node `id`, if it is in the graph.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.slots
            .get(id.index())
            .filter(|slot| slot.generation == id.generation)?
            .node
            .as_ref()
    }

    /// Node `id`, to change its parameters ([`Node::set_param`]).
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let node = self
            .slots
            .get_mut(id.index())
            .filter(|slot| slot.generation == id.generation)?
            .node
            .as_mut()?;
        self.revision += 1;
        Some(node)
    }

    /// Every node with its id, by id.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.slots.iter().zip(0..).filter_map(|(slot, index)| {
            let id = NodeId {
                index,
                generation: slot.generation,
            };
            slot.node.as_ref().map(|node| (id, node))
        })
    }

    /// Socket `index` on `direction` of node `node`, if both exist.
    fn socket(
        &self,
        node: NodeId,
        direction: SocketDirection,
        index: usize,
    ) -> Result<&NodeSocket, GraphError> {
        self.node(node)
            .ok_or(GraphError::MissingNode(node))?
            .sockets(direction)
            .get(index)
            .ok_or(GraphError::MissingSocket {
                node,
                direction,
                index,
            })
    }

    /// Connects output `from` to input `to`, replacing whatever fed `to` before
    /// (returned). Fails, changing nothing, if either socket doesn't exist, the
    /// output can't feed the input ([`NodeSocket::can_feed`]) or the connection
    /// would close a cycle.
    pub fn connect(
        &mut self,
        from: OutputRef,
        to: InputRef,
    ) -> Result<Option<OutputRef>, GraphError> {
        let output = self.socket(from.node, SocketDirection::Output, from.index)?;
        let input = self.socket(to.node, SocketDirection::Input, to.index)?;
        if !output.can_feed(input) {
            return Err(GraphError::Incompatible);
        }
        if self.reaches(to.node, from.node) {
            return Err(GraphError::Cycle);
        }
        self.revision += 1;
        Ok(self.connections.insert(to, from))
    }

    /// Removes the connection into input `to`; returns its source, if it had one.
    pub fn disconnect(&mut self, to: InputRef) -> Option<OutputRef> {
        let source = self.connections.remove(&to)?;
        self.revision += 1;
        Some(source)
    }

    /// Removes every connection from output `from`; returns how many there were.
    pub fn disconnect_output(&mut self, from: OutputRef) -> usize {
        let before = self.connections.len();
        self.connections.retain(|_, source| *source != from);
        let removed = before - self.connections.len();
        if removed > 0 {
            self.revision += 1;
        }
        removed
    }

    /// The output feeding input `to`, if any.
    pub fn source(&self, to: InputRef) -> Option<OutputRef> {
        self.connections.get(&to).copied()
    }

    /// Every connection as `(output, input)`, by input.
    pub fn connections(&self) -> impl Iterator<Item = (OutputRef, InputRef)> {
        self.connections.iter().map(|(&to, &from)| (from, to))
    }

    /// Number of connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Whether `target` can be reached from `start` by following connections
    /// downstream (output → input). A node reaches itself.
    pub fn reaches(&self, start: NodeId, target: NodeId) -> bool {
        let mut stack = vec![start];
        let mut visited = HashSet::new();
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if visited.insert(node) {
                stack.extend(
                    self.connections
                        .iter()
                        .filter(|(_, from)| from.node == node)
                        .map(|(to, _)| to.node),
                );
            }
        }
        false
    }

    /// Every node in processing order: each after all the nodes that feed it.
    /// Among nodes that are ready at the same time, the lowest id goes first, so
    /// the order is deterministic.
    pub fn topo_order(&self) -> Vec<NodeId> {
        let mut pending: BTreeMap<NodeId, usize> = self.nodes().map(|(id, _)| (id, 0)).collect();
        // Several connections between the same two nodes count once each; they
        // are also released once each below, so the counts match.
        for to in self.connections.keys() {
            *pending
                .get_mut(&to.node)
                .expect("connections refer to nodes") += 1;
        }
        let mut ready: BTreeSet<NodeId> = pending
            .iter()
            .filter(|&(_, &count)| count == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut order = Vec::with_capacity(self.len);
        while let Some(node) = ready.pop_first() {
            order.push(node);
            for (to, _) in self
                .connections
                .iter()
                .filter(|(_, from)| from.node == node)
            {
                let count = pending
                    .get_mut(&to.node)
                    .expect("connections refer to nodes");
                *count -= 1;
                if *count == 0 {
                    ready.insert(to.node);
                }
            }
        }
        debug_assert_eq!(order.len(), self.len, "the graph has no cycles");
        order
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{NodeKind, NodeSpec, ParamSpec, ParamValue, SignalRate};

    /// A node with `inputs` inputs and one output.
    fn node(inputs: usize) -> Node {
        let mut spec = NodeSpec::new("test.node", "Node", NodeKind::Processor)
            .output(NodeSocket::signal_out("out"))
            .param(ParamSpec::number("gain", "Gain", 0.0, 2.0, 1.0));
        for i in 0..inputs {
            spec = spec.input(NodeSocket::signal_in(format!("in {i}")));
        }
        Node::new(Arc::new(spec))
    }

    fn out(node: NodeId) -> OutputRef {
        OutputRef::new(node, 0)
    }

    fn input(node: NodeId, index: usize) -> InputRef {
        InputRef::new(node, index)
    }

    /// A graph of `count` nodes with two inputs each.
    fn graph(count: usize) -> (Graph, Vec<NodeId>) {
        let mut graph = Graph::new();
        let ids = (0..count).map(|_| graph.add(node(2))).collect();
        (graph, ids)
    }

    #[test]
    fn removed_ids_stay_dead_when_the_slot_is_reused() {
        let (mut graph, ids) = graph(3);
        assert_eq!(
            ids.iter().map(|id| id.index()).collect::<Vec<_>>(),
            [0, 1, 2]
        );

        assert!(graph.remove(ids[1]).is_some());
        assert_eq!(graph.len(), 2);
        assert!(graph.remove(ids[1]).is_none(), "removed twice");

        let reused = graph.add(node(0));
        assert_eq!(reused.index(), 1, "the lowest free slot is reused");
        assert_ne!(reused, ids[1]);
        assert!(!graph.contains(ids[1]));
        assert!(graph.node(ids[1]).is_none());
        assert!(graph.remove(ids[1]).is_none());
        assert!(graph.contains(reused));
        assert_eq!(
            graph.nodes().map(|(id, _)| id).collect::<Vec<_>>(),
            [ids[0], reused, ids[2]]
        );
    }

    #[test]
    fn node_mut_changes_parameters_through_the_node() {
        let (mut graph, ids) = graph(1);

        graph
            .node_mut(ids[0])
            .unwrap()
            .set_param("gain", ParamValue::Number(2.0))
            .unwrap();

        assert_eq!(
            graph.node(ids[0]).unwrap().param("gain"),
            Some(ParamValue::Number(2.0))
        );
    }

    #[test]
    fn connect_replaces_the_source_of_an_input() {
        let (mut graph, ids) = graph(3);

        assert_eq!(graph.connect(out(ids[0]), input(ids[2], 0)), Ok(None));
        assert_eq!(
            graph.connect(out(ids[1]), input(ids[2], 0)),
            Ok(Some(out(ids[0])))
        );

        assert_eq!(graph.connection_count(), 1);
        assert_eq!(graph.source(input(ids[2], 0)), Some(out(ids[1])));
    }

    #[test]
    fn connect_refuses_missing_nodes_and_sockets() {
        let (mut graph, ids) = graph(2);
        let removed = graph.add(node(2));
        graph.remove(removed);

        assert_eq!(
            graph.connect(out(removed), input(ids[1], 0)),
            Err(GraphError::MissingNode(removed))
        );
        assert_eq!(
            graph.connect(OutputRef::new(ids[0], 1), input(ids[1], 0)),
            Err(GraphError::MissingSocket {
                node: ids[0],
                direction: SocketDirection::Output,
                index: 1
            })
        );
        assert!(matches!(
            graph.connect(out(ids[0]), input(ids[1], 2)),
            Err(GraphError::MissingSocket { .. })
        ));
        assert_eq!(graph.connection_count(), 0);
    }

    #[test]
    fn connect_refuses_sockets_that_dont_fit() {
        let (mut graph, ids) = graph(1);
        let sockets = |output: NodeSocket| {
            let spec = NodeSpec::new("test.source", "Source", NodeKind::Generator).output(output);
            Node::new(Arc::new(spec))
        };
        let control = graph.add(sockets(NodeSocket::output("out", SignalRate::Control)));
        let stereo = graph.add(sockets(NodeSocket::signal_out("out").with_channels(2)));

        assert_eq!(
            graph.connect(out(control), input(ids[0], 0)),
            Err(GraphError::Incompatible)
        );
        // Many channels into mono are averaged: allowed.
        assert_eq!(graph.connect(out(stereo), input(ids[0], 0)), Ok(None));
        assert_eq!(graph.connection_count(), 1);
    }

    #[test]
    fn connect_refuses_cycles() {
        let (mut graph, ids) = graph(3);

        assert_eq!(
            graph.connect(out(ids[0]), input(ids[0], 0)),
            Err(GraphError::Cycle)
        );
        graph.connect(out(ids[0]), input(ids[1], 0)).unwrap();
        graph.connect(out(ids[1]), input(ids[2], 0)).unwrap();
        assert_eq!(
            graph.connect(out(ids[2]), input(ids[0], 0)),
            Err(GraphError::Cycle)
        );
        // A diamond is not a cycle.
        assert!(graph.connect(out(ids[0]), input(ids[2], 1)).is_ok());
        assert_eq!(graph.connection_count(), 3);
    }

    #[test]
    fn disconnecting_and_removing_drop_connections() {
        let (mut graph, ids) = graph(3);
        graph.connect(out(ids[0]), input(ids[1], 0)).unwrap();
        graph.connect(out(ids[0]), input(ids[2], 0)).unwrap();
        graph.connect(out(ids[1]), input(ids[2], 1)).unwrap();

        assert_eq!(graph.disconnect(input(ids[2], 0)), Some(out(ids[0])));
        assert_eq!(graph.disconnect(input(ids[2], 0)), None);
        assert_eq!(graph.disconnect_output(out(ids[0])), 1);

        graph.remove(ids[1]);
        assert_eq!(graph.connection_count(), 0);
    }

    #[test]
    fn topo_order_puts_sources_first() {
        let (mut graph, ids) = graph(4);
        // 3 → 1 → 0, 3 → 0, 2 alone.
        graph.connect(out(ids[3]), input(ids[1], 0)).unwrap();
        graph.connect(out(ids[1]), input(ids[0], 0)).unwrap();
        graph.connect(out(ids[3]), input(ids[0], 1)).unwrap();

        assert_eq!(graph.topo_order(), [ids[2], ids[3], ids[1], ids[0]]);
        assert!(Graph::new().topo_order().is_empty());
    }
}

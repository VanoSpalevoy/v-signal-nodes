//! Compiling a [`Graph`] into a [`Plan`]: what executors run instead of the graph.
//!
//! The graph is the editing model; executors never walk it. [`compile`] turns it
//! into a flat, immutable list of [`Step`]s in processing order, each naming the
//! buffers it reads and writes. Both executors (real time and one pass) run the
//! same plan, so connection rules, order and buffers can't drift apart between
//! them. Compiling needs no device or real-time thread and is tested on its own.
//!
//! Buffers: one per connected-or-not output socket, holding that socket's
//! channels one after another ([`NodeSocket::buffer_len`]). Unconnected inputs
//! read [`Input::Silence`]: the executor keeps one zeroed buffer for all of them.
//! A mono output feeding an N-channel input, or the other way round (allowed by
//! [`NodeSocket::can_feed`]), reads through an extra buffer of the input's shape
//! ([`Input::Converted`]): the executor copies mono into every channel, or
//! averages the channels into mono, before the step runs.
//!
//! Not done yet (TODO P1-1):
//! - reusing buffers once their last reader has run (fewer, cache-warm buffers);
//! - one converted buffer per source and shape instead of one per input;
//! - dropping steps that feed nothing that reaches an output node.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::{
    AppError, Graph, InputRef, NodeId, NodeSocket, NodeSpec, OutputRef, ParamValue, SocketDirection,
};

/// Index of a buffer in a [`Plan`] (see [`Plan::buffers`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferId(usize);

impl BufferId {
    pub fn index(self) -> usize {
        self.0
    }
}

/// Where one input of a step reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The buffer an earlier step's output writes.
    Buffer(BufferId),
    /// Nothing is connected: zeros of the input socket's buffer length.
    Silence,
    /// `source` has a different channel count (one side mono): the executor fills
    /// `buffer`, shaped like the input socket, from it before the step runs.
    Converted { source: BufferId, buffer: BufferId },
}

/// One node to process: its type, parameter values and buffers, in socket order.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub node: NodeId,
    pub spec: Arc<NodeSpec>,
    /// Parameter values as of compiling, in spec order. Executors may change
    /// them in place (parameter messages) without recompiling.
    pub params: Vec<ParamValue>,
    /// One per input socket.
    pub inputs: Vec<Input>,
    /// One per output socket; written only by this step.
    pub outputs: Vec<BufferId>,
}

/// A compiled graph: steps in processing order and the buffers between them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    steps: Vec<Step>,
    /// The socket each buffer holds (its rate and channels decide the buffer's
    /// length), by [`BufferId`]: an output socket, or for a converted buffer the
    /// input socket it feeds.
    buffers: Vec<NodeSocket>,
    /// The buffer of every output socket.
    by_output: HashMap<OutputRef, BufferId>,
}

impl Plan {
    /// Steps in processing order: every step after the steps it reads from.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Mutable steps, for changing parameter values in place.
    pub fn steps_mut(&mut self) -> &mut [Step] {
        &mut self.steps
    }

    /// The socket shape of every buffer, by [`BufferId`].
    pub fn buffers(&self) -> &[NodeSocket] {
        &self.buffers
    }

    /// The buffer output `output` writes, if its node is in the plan.
    pub fn buffer_of(&self, output: OutputRef) -> Option<BufferId> {
        self.by_output.get(&output).copied()
    }
}

/// Why a graph can't be compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The output can't feed the input: different rates, or channel counts that
    /// differ with neither side mono ([`NodeSocket::can_feed`]). [`Graph::connect`]
    /// already refuses such connections; this guards against sockets that change
    /// after connecting (node settings, P1-2).
    Incompatible { from: OutputRef, to: InputRef },
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incompatible { from, to } => write!(
                f,
                "output {} of node {} can't feed input {} of node {}",
                from.index, from.node, to.index, to.node
            ),
        }
    }
}

impl std::error::Error for CompileError {}

impl From<CompileError> for AppError {
    fn from(err: CompileError) -> Self {
        Self::InvalidInput(err.to_string())
    }
}

/// Compiles `graph` into a [`Plan`]: steps in [`Graph::topo_order`], a buffer per
/// output socket, silence for unconnected inputs, a converted buffer where a mono
/// side meets several channels. Fails on the first connection
/// whose sockets don't fit (see [`CompileError`]).
pub fn compile(graph: &Graph) -> Result<Plan, CompileError> {
    let mut plan = Plan::default();
    let order = graph.topo_order();

    // Buffers first, so every step can name the buffers of its sources.
    for &id in &order {
        let node = graph.node(id).expect("topo_order lists existing nodes");
        for (index, socket) in node.sockets(SocketDirection::Output).iter().enumerate() {
            plan.by_output
                .insert(OutputRef::new(id, index), BufferId(plan.buffers.len()));
            plan.buffers.push(socket.clone());
        }
    }

    for id in order {
        let node = graph.node(id).expect("topo_order lists existing nodes");
        let mut inputs = Vec::with_capacity(node.sockets(SocketDirection::Input).len());
        for (index, socket) in node.sockets(SocketDirection::Input).iter().enumerate() {
            let to = InputRef::new(id, index);
            let Some(from) = graph.source(to) else {
                inputs.push(Input::Silence);
                continue;
            };
            // A source socket that no longer exists can't feed anything either.
            let Some(&buffer) = plan.by_output.get(&from) else {
                return Err(CompileError::Incompatible { from, to });
            };
            let source = &plan.buffers[buffer.0];
            if !source.can_feed(socket) {
                return Err(CompileError::Incompatible { from, to });
            }
            if source.channels == socket.channels {
                inputs.push(Input::Buffer(buffer));
            } else {
                let converted = BufferId(plan.buffers.len());
                plan.buffers.push(socket.clone());
                inputs.push(Input::Converted {
                    source: buffer,
                    buffer: converted,
                });
            }
        }
        let outputs = (0..node.sockets(SocketDirection::Output).len())
            .map(|index| plan.by_output[&OutputRef::new(id, index)])
            .collect();
        plan.steps.push(Step {
            node: id,
            spec: Arc::clone(node.spec()),
            params: node.params().to_vec(),
            inputs,
            outputs,
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Node, NodeKind};

    fn spec(inputs: &[NodeSocket], outputs: &[NodeSocket]) -> Arc<NodeSpec> {
        let mut spec = NodeSpec::new("test.node", "Node", NodeKind::Processor);
        for socket in inputs {
            spec = spec.input(socket.clone());
        }
        for socket in outputs {
            spec = spec.output(socket.clone());
        }
        Arc::new(spec)
    }

    fn mono() -> NodeSocket {
        NodeSocket::signal_out("out")
    }

    #[test]
    fn steps_follow_the_signal_and_unconnected_inputs_are_silent() {
        let mut graph = Graph::new();
        let sink = graph.add(Node::new(spec(&[mono(), mono()], &[])));
        let source = graph.add(Node::new(spec(&[], &[mono()])));
        graph
            .connect(OutputRef::new(source, 0), InputRef::new(sink, 0))
            .unwrap();

        let plan = compile(&graph).unwrap();

        let nodes: Vec<_> = plan.steps().iter().map(|step| step.node).collect();
        assert_eq!(nodes, [source, sink]);
        let buffer = plan.buffer_of(OutputRef::new(source, 0)).unwrap();
        assert_eq!(plan.steps()[0].outputs, [buffer]);
        assert_eq!(
            plan.steps()[1].inputs,
            [Input::Buffer(buffer), Input::Silence]
        );
        assert_eq!(plan.buffers().len(), 1);
    }

    /// A graph with one connection from `output` to `input`, and its ends.
    fn pair(output: NodeSocket, input: NodeSocket) -> (Graph, OutputRef, InputRef) {
        let mut graph = Graph::new();
        let source = graph.add(Node::new(spec(&[], &[output])));
        let sink = graph.add(Node::new(spec(&[input], &[])));
        let (from, to) = (OutputRef::new(source, 0), InputRef::new(sink, 0));
        graph.connect(from, to).unwrap();
        (graph, from, to)
    }

    #[test]
    fn mono_meets_channels_through_a_converted_buffer() {
        let stereo_in = NodeSocket::signal_in("in").with_channels(2);
        let (graph, from, _) = pair(mono(), stereo_in.clone());

        let plan = compile(&graph).unwrap();

        let source = plan.buffer_of(from).unwrap();
        let Input::Converted {
            source: read,
            buffer,
        } = plan.steps()[1].inputs[0]
        else {
            panic!("not converted: {:?}", plan.steps()[1].inputs);
        };
        assert_eq!(read, source);
        assert_eq!(plan.buffers()[buffer.index()], stereo_in);
    }
}

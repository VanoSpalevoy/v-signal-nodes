//! Compiling a [`Graph`] into a [`Plan`]: what executors run instead of the graph.
//!
//! The graph is the editing model; executors never walk it. [`compile`] turns it
//! into a flat, immutable list of [`Step`]s in processing order, each naming the
//! buffers it reads and writes. Both executors (real time and one pass) run the
//! same plan, so connection rules, order and buffers can't drift apart between
//! them. Compiling needs no device or real-time thread and is tested on its own.
//!
//! What runs: only nodes that reach an output node ([`NodeKind::Output`]); the rest
//! produce nothing anyone reads. A graph without output nodes doesn't compile
//! ([`CompileError::NoOutput`]). [`Plan::outputs`] lists what the output nodes
//! read, for whoever delivers it (a device driver, a file, a display).
//!
//! Buffers: a socket's channels one after another
//! ([`BufferShape::buffer_len`]). Unconnected inputs read [`Input::Silence`]: the
//! executor keeps one zeroed buffer for all of them. A mono output feeding an
//! N-channel input, or the other way round (allowed by [`NodeSocket::can_feed`]),
//! reads through an extra buffer of the input's shape, filled once per block by the
//! first step that needs it ([`Conversion`]) and shared by all readers of that
//! source and shape.
//!
//! A buffer is reused once its last reader has run, by a later writer of the same
//! shape, so a block touches fewer, cache-warm buffers. Only what the output nodes
//! read stays valid after the block ([`Plan::buffer_of`]). Reusing makes the order
//! matter beyond the signal flow, so every step lists all the earlier steps it must
//! follow ([`Step::after`]); running steps in any order that respects those lists,
//! in parallel too, gives the same result as running them one by one.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use crate::{
    Graph, InputRef, NodeId, NodeKind, NodeSocket, NodeSpec, OutputRef, ParamValue, SignalRate,
    SocketDirection,
};

/// Index of a buffer in a [`Plan`] (see [`Plan::buffers`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferId(usize);

impl BufferId {
    pub fn index(self) -> usize {
        self.0
    }
}

/// What a buffer holds; the same shape means the same length in any block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferShape {
    pub rate: SignalRate,
    pub channels: u16,
}

impl BufferShape {
    /// The shape of `socket`'s buffer.
    pub fn of(socket: &NodeSocket) -> Self {
        Self {
            rate: socket.rate,
            channels: socket.channels,
        }
    }

    /// Values per channel in a block of `frames` ([`NodeSocket::channel_len`]).
    pub fn channel_len(self, frames: usize) -> usize {
        match self.rate {
            SignalRate::Sample => frames,
            SignalRate::Control => 1,
        }
    }

    /// Length of the buffer in a block of `frames` ([`NodeSocket::buffer_len`]).
    pub fn buffer_len(self, frames: usize) -> usize {
        self.channel_len(frames) * usize::from(self.channels)
    }
}

/// Where one input of a step reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A buffer written earlier in the block: by an earlier step's output, or by a
    /// [`Conversion`].
    Buffer(BufferId),
    /// Nothing is connected: zeros of the input socket's buffer length.
    Silence,
}

/// Fills `buffer` from `source` before a step runs. One of the two is mono: mono is
/// copied into every channel, many channels are averaged into mono.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conversion {
    pub source: BufferId,
    pub buffer: BufferId,
}

/// One node to process: its type, parameter values and buffers, in socket order.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub node: NodeId,
    pub spec: Arc<NodeSpec>,
    /// Parameter values as of compiling, in spec order. Executors may change
    /// them in place (parameter messages) without recompiling.
    pub params: Vec<ParamValue>,
    /// To do before the processor runs, in order.
    pub conversions: Vec<Conversion>,
    /// One per input socket.
    pub inputs: Vec<Input>,
    /// One per output socket; written only by this step until its readers are done.
    pub outputs: Vec<BufferId>,
    /// Indices of the earlier steps that must be finished before this one starts,
    /// sorted: those it reads from and those that use a buffer it takes over.
    pub after: Vec<usize>,
}

/// One input of an output node: a signal the plan delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanOutput {
    pub node: NodeId,
    /// The input socket's index.
    pub input: usize,
    pub source: Input,
    pub shape: BufferShape,
}

/// A compiled graph: steps in processing order and the buffers between them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    steps: Vec<Step>,
    /// The shape of each buffer, by [`BufferId`].
    buffers: Vec<BufferShape>,
    /// The buffers still valid after a block: of the outputs feeding output nodes.
    by_output: HashMap<OutputRef, BufferId>,
    outputs: Vec<PlanOutput>,
}

impl Plan {
    /// Steps in processing order: every step after the steps in its
    /// [`after`](Step::after).
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Mutable steps, for changing parameter values in place.
    pub fn steps_mut(&mut self) -> &mut [Step] {
        &mut self.steps
    }

    /// The shape of every buffer, by [`BufferId`].
    pub fn buffers(&self) -> &[BufferShape] {
        &self.buffers
    }

    /// The buffer holding what `output` wrote, if it stays valid after the block:
    /// only outputs feeding an output node do, the others may be overwritten.
    pub fn buffer_of(&self, output: OutputRef) -> Option<BufferId> {
        self.by_output.get(&output).copied()
    }

    /// Every input of every output node, in step and then socket order.
    pub fn outputs(&self) -> &[PlanOutput] {
        &self.outputs
    }
}

/// Why a graph can't be compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// No node is an output ([`NodeKind::Output`]), so nothing would be delivered.
    NoOutput,
    /// The output can't feed the input: different rates, or channel counts that
    /// differ with neither side mono ([`NodeSocket::can_feed`]). [`Graph::connect`]
    /// already refuses such connections; this guards against sockets that change
    /// after connecting (node settings, P1-2).
    Incompatible { from: OutputRef, to: InputRef },
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutput => f.write_str("the graph has no output node"),
            Self::Incompatible { from, to } => write!(
                f,
                "output {} of node {} can't feed input {} of node {}",
                from.index, from.node, to.index, to.node
            ),
        }
    }
}

impl std::error::Error for CompileError {}

/// A buffer before physical buffers are assigned: one written value per block.
struct Logical {
    shape: BufferShape,
    /// The step writing it.
    writer: usize,
    /// Every step reading it, in order.
    readers: Vec<usize>,
    /// Read after the block: never reused.
    pinned: bool,
}

impl Logical {
    /// The last step that touches it.
    fn last_use(&self) -> usize {
        self.readers.last().copied().unwrap_or(self.writer)
    }
}

/// Compiles `graph` into a [`Plan`]: steps of the nodes that reach an output node,
/// in [`Graph::topo_order`], with buffers reused by lifetime (see the module docs).
/// Fails if there is no output node or on the first connection between those nodes
/// whose sockets don't fit (see [`CompileError`]).
pub fn compile(graph: &Graph) -> Result<Plan, CompileError> {
    let kept = reaching_outputs(graph);
    if kept.is_empty() {
        return Err(CompileError::NoOutput);
    }

    let mut logical: Vec<Logical> = Vec::new();
    let mut of_output: HashMap<OutputRef, usize> = HashMap::new();
    let mut converted: HashMap<(usize, BufferShape), usize> = HashMap::new();
    let mut steps = Vec::with_capacity(kept.len());
    let mut pinned_outputs = Vec::new();

    // Everything below uses logical indices as `BufferId`s; `assign_buffers` then
    // swaps them for the physical ones.
    let order = graph
        .topo_order()
        .into_iter()
        .filter(|id| kept.contains(id));
    for (step, id) in order.enumerate() {
        let node = graph.node(id).expect("topo_order lists existing nodes");
        let is_output = node.spec().kind == NodeKind::Output;
        let sockets = node.sockets(SocketDirection::Input);
        let mut inputs = Vec::with_capacity(sockets.len());
        let mut conversions = Vec::new();
        for (index, socket) in sockets.iter().enumerate() {
            let to = InputRef::new(id, index);
            let Some(from) = graph.source(to) else {
                inputs.push(Input::Silence);
                continue;
            };
            // A source socket that no longer exists can't feed anything either.
            let Some(&source) = of_output.get(&from) else {
                return Err(CompileError::Incompatible { from, to });
            };
            let fits = graph
                .node(from.node)
                .and_then(|node| node.sockets(SocketDirection::Output).get(from.index))
                .is_some_and(|output| output.can_feed(socket));
            if !fits {
                return Err(CompileError::Incompatible { from, to });
            }
            let shape = BufferShape::of(socket);
            let read = if logical[source].shape == shape {
                source
            } else {
                *converted.entry((source, shape)).or_insert_with(|| {
                    logical[source].readers.push(step);
                    logical.push(Logical {
                        shape,
                        writer: step,
                        readers: Vec::new(),
                        pinned: false,
                    });
                    let buffer = logical.len() - 1;
                    conversions.push(Conversion {
                        source: BufferId(source),
                        buffer: BufferId(buffer),
                    });
                    buffer
                })
            };
            logical[read].readers.push(step);
            if is_output {
                logical[read].pinned = true;
                logical[source].pinned = true;
                pinned_outputs.push((from, source));
            }
            inputs.push(Input::Buffer(BufferId(read)));
        }
        let mut outputs = Vec::new();
        for (index, socket) in node.sockets(SocketDirection::Output).iter().enumerate() {
            let buffer = logical.len();
            logical.push(Logical {
                shape: BufferShape::of(socket),
                writer: step,
                readers: Vec::new(),
                pinned: false,
            });
            of_output.insert(OutputRef::new(id, index), buffer);
            outputs.push(BufferId(buffer));
        }
        steps.push(Step {
            node: id,
            spec: Arc::clone(node.spec()),
            params: node.params().to_vec(),
            conversions,
            inputs,
            outputs,
            after: Vec::new(),
        });
    }

    let (buffers, physical) = assign_buffers(&mut steps, &logical);
    let by_output = pinned_outputs
        .into_iter()
        .map(|(from, source)| (from, physical[source]))
        .collect();
    let outputs = steps
        .iter()
        .filter(|step| step.spec.kind == NodeKind::Output)
        .flat_map(|step| {
            let sockets = step.spec.sockets(SocketDirection::Input);
            step.inputs
                .iter()
                .zip(sockets)
                .enumerate()
                .map(|(input, (&source, socket))| PlanOutput {
                    node: step.node,
                    input,
                    source,
                    shape: BufferShape::of(socket),
                })
        })
        .collect();
    Ok(Plan {
        steps,
        buffers,
        by_output,
        outputs,
    })
}

/// The nodes with a path to an output node, output nodes included.
fn reaching_outputs(graph: &Graph) -> HashSet<NodeId> {
    let mut stack: Vec<NodeId> = graph
        .nodes()
        .filter(|(_, node)| node.spec().kind == NodeKind::Output)
        .map(|(id, _)| id)
        .collect();
    let mut kept = HashSet::new();
    while let Some(id) = stack.pop() {
        if kept.insert(id) {
            let node = graph.node(id).expect("listed nodes exist");
            let inputs = node.sockets(SocketDirection::Input).len();
            stack.extend(
                (0..inputs).filter_map(|index| Some(graph.source(InputRef::new(id, index))?.node)),
            );
        }
    }
    kept
}

/// Maps `logical` buffers to physical ones, reusing a free buffer of the same shape,
/// and fills in every [`Step::after`]. `steps` come with logical ids and leave with
/// physical ones. Returns the physical buffers' shapes and the logical → physical
/// map.
fn assign_buffers(steps: &mut [Step], logical: &[Logical]) -> (Vec<BufferShape>, Vec<BufferId>) {
    let mut shapes: Vec<BufferShape> = Vec::new();
    let mut physical = vec![BufferId(0); logical.len()];
    let mut free: HashMap<BufferShape, Vec<BufferId>> = HashMap::new();
    // The steps touching what each physical buffer holds now.
    let mut users: Vec<Vec<usize>> = Vec::new();
    let mut released_by: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    for (id, buffer) in logical.iter().enumerate() {
        if !buffer.pinned {
            released_by[buffer.last_use()].push(id);
        }
    }

    for (index, step) in steps.iter_mut().enumerate() {
        let mut after = Vec::new();
        // Reads come first: they name buffers written by earlier steps, or by this
        // one's conversions, which are mapped next.
        for read in step
            .conversions
            .iter()
            .map(|conversion| conversion.source)
            .chain(step.inputs.iter().filter_map(|input| match input {
                Input::Buffer(buffer) => Some(*buffer),
                Input::Silence => None,
            }))
        {
            after.push(logical[read.0].writer);
        }
        // A buffer taken over must wait for everyone using its previous content.
        let written = step
            .conversions
            .iter_mut()
            .map(|conversion| &mut conversion.buffer)
            .chain(step.outputs.iter_mut());
        for buffer in written {
            let id = buffer.0;
            let shape = logical[id].shape;
            let target = match free.get_mut(&shape).and_then(Vec::pop) {
                Some(target) => {
                    after.append(&mut users[target.0]);
                    target
                }
                None => {
                    shapes.push(shape);
                    users.push(Vec::new());
                    BufferId(shapes.len() - 1)
                }
            };
            users[target.0].push(index);
            users[target.0].extend(&logical[id].readers);
            physical[id] = target;
            *buffer = target;
        }
        for conversion in &mut step.conversions {
            conversion.source = physical[conversion.source.0];
        }
        for input in &mut step.inputs {
            if let Input::Buffer(buffer) = input {
                *buffer = physical[buffer.0];
            }
        }
        after.retain(|&earlier| earlier != index);
        after.sort_unstable();
        after.dedup();
        step.after = after;
        for &id in &released_by[index] {
            free.entry(logical[id].shape)
                .or_default()
                .push(physical[id]);
        }
    }
    (shapes, physical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Node;

    fn spec(kind: NodeKind, inputs: &[NodeSocket], outputs: &[NodeSocket]) -> Arc<NodeSpec> {
        let mut spec = NodeSpec::new("test.node", "Node", kind);
        for socket in inputs {
            spec = spec.input(socket.clone());
        }
        for socket in outputs {
            spec = spec.output(socket.clone());
        }
        Arc::new(spec)
    }

    fn mono_in() -> NodeSocket {
        NodeSocket::signal_in("in")
    }

    fn mono_out() -> NodeSocket {
        NodeSocket::signal_out("out")
    }

    fn source(graph: &mut Graph) -> NodeId {
        graph.add(Node::new(spec(NodeKind::Generator, &[], &[mono_out()])))
    }

    fn thru(graph: &mut Graph) -> NodeId {
        let spec = spec(NodeKind::Processor, &[mono_in()], &[mono_out()]);
        graph.add(Node::new(spec))
    }

    fn sink(graph: &mut Graph, input: NodeSocket) -> NodeId {
        graph.add(Node::new(spec(NodeKind::Output, &[input], &[])))
    }

    fn connect(graph: &mut Graph, from: NodeId, to: NodeId) {
        graph
            .connect(OutputRef::new(from, 0), InputRef::new(to, 0))
            .unwrap();
    }

    /// Checks the promise of [`Step::after`]: following those lists alone, a step
    /// never runs while another one uses the same buffer. Two steps sharing a buffer
    /// must be ordered through the lists, unless both only read it.
    fn assert_buffers_are_ordered(plan: &Plan) {
        let steps = plan.steps();
        // Everything each step is guaranteed to follow.
        let mut before: Vec<HashSet<usize>> = Vec::new();
        for step in steps {
            let mut all = HashSet::new();
            for &earlier in &step.after {
                all.insert(earlier);
                all.extend(&before[earlier]);
            }
            before.push(all);
        }
        let reads = |step: &Step| -> Vec<BufferId> {
            let converted = step.conversions.iter().map(|c| c.source);
            let inputs = step.inputs.iter().filter_map(|input| match input {
                Input::Buffer(buffer) => Some(*buffer),
                Input::Silence => None,
            });
            converted.chain(inputs).collect()
        };
        let writes = |step: &Step| -> Vec<BufferId> {
            let converted = step.conversions.iter().map(|c| c.buffer);
            converted.chain(step.outputs.iter().copied()).collect()
        };
        for (later, b) in steps.iter().enumerate() {
            for (earlier, a) in steps[..later].iter().enumerate() {
                let conflict = writes(b)
                    .iter()
                    .any(|buffer| writes(a).contains(buffer) || reads(a).contains(buffer))
                    || writes(a).iter().any(|buffer| reads(b).contains(buffer));
                assert!(
                    !conflict || before[later].contains(&earlier),
                    "steps {earlier} and {later} share a buffer unordered: {plan:#?}"
                );
            }
        }
    }

    #[test]
    fn steps_follow_the_signal_and_unconnected_inputs_are_silent() {
        let mut graph = Graph::new();
        let output = spec(
            NodeKind::Output,
            &[mono_in(), NodeSocket::signal_in("b")],
            &[],
        );
        let sink = graph.add(Node::new(output));
        let source = source(&mut graph);
        connect(&mut graph, source, sink);

        let plan = compile(&graph).unwrap();

        let nodes: Vec<_> = plan.steps().iter().map(|step| step.node).collect();
        assert_eq!(nodes, [source, sink]);
        let buffer = plan.buffer_of(OutputRef::new(source, 0)).unwrap();
        assert_eq!(plan.steps()[0].outputs, [buffer]);
        assert_eq!(
            plan.steps()[1].inputs,
            [Input::Buffer(buffer), Input::Silence]
        );
        assert_eq!(plan.steps()[1].after, [0]);
        assert_eq!(plan.buffers().len(), 1);
        assert_eq!(plan.outputs().len(), 2);
        assert_eq!(plan.outputs()[0].source, Input::Buffer(buffer));
        assert_eq!(plan.outputs()[1].source, Input::Silence);
    }

    #[test]
    fn needs_an_output_node() {
        let mut graph = Graph::new();
        assert_eq!(compile(&graph), Err(CompileError::NoOutput));

        let source = source(&mut graph);
        let thru = thru(&mut graph);
        connect(&mut graph, source, thru);
        assert_eq!(compile(&graph), Err(CompileError::NoOutput));
    }

    #[test]
    fn steps_not_reaching_an_output_are_dropped() {
        let mut graph = Graph::new();
        let used = source(&mut graph);
        let tail = thru(&mut graph);
        let unused = source(&mut graph);
        let sink = sink(&mut graph, mono_in());
        connect(&mut graph, used, sink);
        // A branch off the used source that leads nowhere.
        connect(&mut graph, used, tail);

        let plan = compile(&graph).unwrap();

        let nodes: Vec<_> = plan.steps().iter().map(|step| step.node).collect();
        assert_eq!(nodes, [used, sink]);
        assert!(plan.buffer_of(OutputRef::new(unused, 0)).is_none());
    }

    #[test]
    fn buffers_are_reused_along_a_chain() {
        let mut graph = Graph::new();
        let first = source(&mut graph);
        let mut last = first;
        for _ in 0..8 {
            let next = thru(&mut graph);
            connect(&mut graph, last, next);
            last = next;
        }
        let sink = sink(&mut graph, mono_in());
        connect(&mut graph, last, sink);

        let plan = compile(&graph).unwrap();

        assert_eq!(plan.steps().len(), 10);
        assert_eq!(plan.buffers().len(), 2, "one to read, one to write");
        assert!(plan.buffer_of(OutputRef::new(last, 0)).is_some());
        assert!(
            plan.buffer_of(OutputRef::new(first, 0)).is_none(),
            "overwritten later, so not readable after the block"
        );
        assert_buffers_are_ordered(&plan);
    }

    #[test]
    fn a_step_never_writes_what_it_reads() {
        let mut graph = Graph::new();
        let source = source(&mut graph);
        let thru = thru(&mut graph);
        let sink = sink(&mut graph, mono_in());
        connect(&mut graph, source, thru);
        connect(&mut graph, thru, sink);

        let plan = compile(&graph).unwrap();

        let step = &plan.steps()[1];
        assert_ne!(step.inputs[0], Input::Buffer(step.outputs[0]));
    }

    #[test]
    fn a_buffer_stays_until_its_last_reader() {
        // source → a and → b, with b run after a's whole branch.
        let mut graph = Graph::new();
        let source = source(&mut graph);
        let a = thru(&mut graph);
        let a2 = thru(&mut graph);
        let b = thru(&mut graph);
        let sink_a = sink(&mut graph, mono_in());
        let sink_b = sink(&mut graph, mono_in());
        connect(&mut graph, source, a);
        connect(&mut graph, a, a2);
        connect(&mut graph, a2, sink_a);
        connect(&mut graph, source, b);
        connect(&mut graph, b, sink_b);

        let plan = compile(&graph).unwrap();

        let read_by_b = plan
            .steps()
            .iter()
            .find(|step| step.node == b)
            .map(|step| step.inputs[0]);
        let written_by_source = plan.steps()[0].outputs[0];
        assert_eq!(read_by_b, Some(Input::Buffer(written_by_source)));
        for step in plan.steps().iter().filter(|step| step.node != source) {
            assert!(
                !step.outputs.contains(&written_by_source)
                    || step
                        .after
                        .contains(&plan.steps().iter().position(|s| s.node == b).unwrap()),
                "{step:?} overwrites the source before b reads it"
            );
        }
        assert_buffers_are_ordered(&plan);
    }

    #[test]
    fn independent_branches_stay_unordered() {
        let mut graph = Graph::new();
        let a = source(&mut graph);
        let b = source(&mut graph);
        let sink_a = sink(&mut graph, mono_in());
        let sink_b = sink(&mut graph, mono_in());
        connect(&mut graph, a, sink_a);
        connect(&mut graph, b, sink_b);

        let plan = compile(&graph).unwrap();

        let of = |node| plan.steps().iter().find(|step| step.node == node).unwrap();
        assert!(of(a).after.is_empty());
        assert!(of(b).after.is_empty());
        assert_buffers_are_ordered(&plan);
    }

    #[test]
    fn taking_over_a_buffer_orders_the_steps() {
        // a's buffer is free once sink-less `mid` has read it; `late` writes the same
        // shape after that, though it has no signal path from `mid`.
        let mut graph = Graph::new();
        let a = source(&mut graph);
        let mid = thru(&mut graph);
        let late = source(&mut graph);
        let sink_mid = sink(&mut graph, mono_in());
        let sink_late = sink(&mut graph, mono_in());
        connect(&mut graph, a, mid);
        connect(&mut graph, mid, sink_mid);
        connect(&mut graph, late, sink_late);

        let plan = compile(&graph).unwrap();

        assert_buffers_are_ordered(&plan);
    }

    /// A graph with one connection from `output` to a sink's `input`.
    fn pair(output: NodeSocket, input: NodeSocket) -> (Graph, OutputRef, InputRef) {
        let mut graph = Graph::new();
        let source = graph.add(Node::new(spec(NodeKind::Generator, &[], &[output])));
        let sink = sink(&mut graph, input);
        let (from, to) = (OutputRef::new(source, 0), InputRef::new(sink, 0));
        graph.connect(from, to).unwrap();
        (graph, from, to)
    }

    #[test]
    fn mono_meets_channels_through_a_converted_buffer() {
        let stereo_in = NodeSocket::signal_in("in").with_channels(2);
        let (graph, from, _) = pair(mono_out(), stereo_in.clone());

        let plan = compile(&graph).unwrap();

        let source = plan.buffer_of(from).unwrap();
        let step = &plan.steps()[1];
        let [conversion] = step.conversions[..] else {
            panic!("not converted: {step:?}");
        };
        assert_eq!(conversion.source, source);
        assert_eq!(step.inputs, [Input::Buffer(conversion.buffer)]);
        assert_eq!(
            plan.buffers()[conversion.buffer.index()],
            BufferShape::of(&stereo_in)
        );
        assert_eq!(plan.outputs()[0].source, step.inputs[0]);
    }

    #[test]
    fn readers_of_a_source_and_shape_share_one_conversion() {
        let stereo_in = NodeSocket::signal_in("in").with_channels(2);
        let mut graph = Graph::new();
        let source = source(&mut graph);
        let first = sink(&mut graph, stereo_in.clone());
        let second = sink(&mut graph, stereo_in.clone());
        connect(&mut graph, source, first);
        connect(&mut graph, source, second);

        let plan = compile(&graph).unwrap();

        let conversions: Vec<_> = plan
            .steps()
            .iter()
            .flat_map(|step| &step.conversions)
            .collect();
        assert_eq!(conversions.len(), 1);
        assert_eq!(plan.steps()[1].inputs, plan.steps()[2].inputs);
        assert_eq!(plan.steps()[2].after, [1], "the first reader fills it");
        assert_buffers_are_ordered(&plan);
    }

    #[test]
    fn two_inputs_of_a_step_share_a_conversion_once() {
        let stereo_in = NodeSocket::signal_in("a").with_channels(2);
        let other = NodeSocket::signal_in("b").with_channels(2);
        let mut graph = Graph::new();
        let source = source(&mut graph);
        let spec = spec(NodeKind::Output, &[stereo_in, other], &[]);
        let sink = graph.add(Node::new(spec));
        for index in 0..2 {
            graph
                .connect(OutputRef::new(source, 0), InputRef::new(sink, index))
                .unwrap();
        }

        let plan = compile(&graph).unwrap();

        let step = &plan.steps()[1];
        assert_eq!(step.conversions.len(), 1);
        assert_eq!(step.inputs[0], step.inputs[1]);
    }

    #[test]
    fn a_control_input_gets_its_own_shape() {
        let (graph, ..) = pair(
            NodeSocket::control_out("out"),
            NodeSocket::control_in("in").with_channels(3),
        );

        let plan = compile(&graph).unwrap();

        let shape = plan.outputs()[0].shape;
        assert_eq!(shape.channel_len(64), 1);
        assert_eq!(shape.buffer_len(64), 3);
    }
}

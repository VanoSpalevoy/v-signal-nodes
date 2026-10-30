//! [`Engine`]: runs a [`Plan`] block by block.
//!
//! All buffers live in one pool allocated up front for `max_frames`; a block of
//! `frames` uses the first [`BufferShape::buffer_len`]`(frames)` samples of each.
//! [`process`](Engine::process) only walks the steps, points each processor at its
//! buffers and calls it: no allocation, no locks (apart from the error path, see
//! [`RuntimeError`]). A panicking processor is caught and reported as an error.
//!
//! [`process_parallel`](Engine::process_parallel) runs the same steps on several
//! threads, each step once those in its [`Step::after`] are done. That order is all
//! the plan needs, so the result is the same as one by one.
//!
//! A new plan keeps the state of the nodes it shares with the old one:
//! [`successor`](Engine::successor) leaves them out and
//! [`adopt`](Engine::adopt) moves their processors over, without allocating.
//!
//! TODO(P1-2): steps should carry their own sockets once nodes' settings can change
//! them (`core::Node::sockets`); for now the spec's are used.

use std::collections::HashMap;
use std::fmt::Display;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;
use std::sync::{Condvar, Mutex};
use std::thread;

use v_signal_nodes_core::{
    BufferId, BufferShape, Input, NodeId, OutputRef, ParamValue, Plan, PlanOutput, Step,
};
use v_signal_nodes_plugin_api::{NodeProcessor, PluginError, ProcessIo, RawBuffer};

use crate::RuntimeError;

/// A plan with one prepared processor per step and the buffers between them.
pub struct Engine {
    plan: Plan,
    /// One per step, in step order.
    processors: Vec<Box<dyn NodeProcessor>>,
    /// Every buffer of the plan, back to back.
    pool: Vec<f32>,
    /// Where each buffer starts in `pool`, by `BufferId`.
    offsets: Vec<usize>,
    /// Zeros for unconnected inputs: as long as the longest input buffer.
    silence: Vec<f32>,
    max_frames: usize,
    /// Scratch for one step's buffer pointers, reused so `process` doesn't allocate.
    inputs: Vec<RawBuffer>,
    outputs: Vec<RawBuffer>,
    /// Processors to take from the previous engine in [`adopt`](Self::adopt), as
    /// `(step there, step here)`.
    carried: Vec<(usize, usize)>,
}

// SAFETY: the only non-`Send` fields are the `RawBuffer` scratch vectors. Their
// pointers are rebuilt from `pool`/`silence` at the start of every step in
// `process` and only dereferenced there, by the processor called within the same
// `&mut self` borrow; a stale pointer left behind is never read. Everything else
// is `Send` (`NodeProcessor: Send`).
unsafe impl Send for Engine {}

/// One processor per step of `plan`, except the nodes in `skip`, created by `make`.
/// For [`Engine::new`] and [`Engine::successor`]; fails on the first `make` that does.
pub fn processors_for<E: Display>(
    plan: &Plan,
    skip: &[NodeId],
    mut make: impl FnMut(&Step) -> Result<Box<dyn NodeProcessor>, E>,
) -> Result<HashMap<NodeId, Box<dyn NodeProcessor>>, RuntimeError> {
    plan.steps()
        .iter()
        .filter(|step| !skip.contains(&step.node))
        .map(|step| {
            let processor = make(step).map_err(|err| RuntimeError::CreateProcessor {
                node: step.node,
                message: err.to_string(),
            })?;
            Ok((step.node, processor))
        })
        .collect()
}

/// Stands in for a processor still in the previous engine until
/// [`Engine::adopt`] brings it over; an engine isn't run before that.
struct Carried;

impl NodeProcessor for Carried {
    fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        Err(PluginError::new("processor wasn't adopted"))
    }
}

impl Engine {
    /// Prepares `plan` for blocks of at most `max_frames` at `sample_rate`: takes
    /// the processor of every step's node from `processors` (the rest are dropped),
    /// allocates the buffers and calls `prepare` on each processor.
    pub fn new(
        plan: Plan,
        processors: HashMap<NodeId, Box<dyn NodeProcessor>>,
        sample_rate: f32,
        max_frames: usize,
    ) -> Result<Self, RuntimeError> {
        Self::successor(plan, &[], processors, sample_rate, max_frames)
    }

    /// Like [`new`](Self::new), for an engine that will replace one running
    /// `previous` (its nodes, in step order), at the same `sample_rate` and
    /// `max_frames`: nodes also in `previous` need no processor here, they keep
    /// theirs through [`adopt`](Self::adopt), and with it their signal history.
    pub fn successor(
        plan: Plan,
        previous: &[NodeId],
        mut processors: HashMap<NodeId, Box<dyn NodeProcessor>>,
        sample_rate: f32,
        max_frames: usize,
    ) -> Result<Self, RuntimeError> {
        let old_steps: HashMap<NodeId, usize> = previous
            .iter()
            .enumerate()
            .map(|(step, &node)| (node, step))
            .collect();
        let mut ordered: Vec<Box<dyn NodeProcessor>> = Vec::with_capacity(plan.steps().len());
        let mut carried = Vec::new();
        for (index, step) in plan.steps().iter().enumerate() {
            if let Some(&old) = old_steps.get(&step.node) {
                carried.push((old, index));
                ordered.push(Box::new(Carried));
                continue;
            }
            let mut processor = processors
                .remove(&step.node)
                .ok_or(RuntimeError::MissingProcessor(step.node))?;
            let prepared = catch_unwind(AssertUnwindSafe(|| {
                processor.prepare(sample_rate, max_frames)
            }));
            match prepared {
                Ok(Ok(())) => ordered.push(processor),
                Ok(Err(error)) => {
                    return Err(RuntimeError::Plugin {
                        node: step.node,
                        error,
                    });
                }
                Err(_) => return Err(RuntimeError::Panicked(step.node)),
            }
        }

        let mut offsets = Vec::with_capacity(plan.buffers().len());
        let mut pool_len = 0;
        for shape in plan.buffers() {
            offsets.push(pool_len);
            pool_len += shape.buffer_len(max_frames);
        }
        let inputs = plan.steps().iter().flat_map(|step| &step.spec.inputs);
        let silence_len = inputs
            .map(|socket| socket.buffer_len(max_frames))
            .max()
            .unwrap_or(0);
        let most = |count: fn(&Step) -> usize| plan.steps().iter().map(count).max().unwrap_or(0);
        let (max_inputs, max_outputs) = (most(|s| s.inputs.len()), most(|s| s.outputs.len()));

        Ok(Self {
            plan,
            processors: ordered,
            pool: vec![0.0; pool_len],
            offsets,
            silence: vec![0.0; silence_len],
            max_frames,
            inputs: Vec::with_capacity(max_inputs),
            outputs: Vec::with_capacity(max_outputs),
            carried,
        })
    }

    /// Takes the processors [`successor`](Self::successor) left out from `previous`,
    /// the engine this one replaces. Allocates nothing, frees nothing and can't
    /// panic, so it's safe on the real-time thread. `previous` must not run again.
    pub fn adopt(&mut self, previous: &mut Engine) {
        for &(old, new) in &self.carried {
            if let (Some(from), Some(to)) = (
                previous.processors.get_mut(old),
                self.processors.get_mut(new),
            ) {
                std::mem::swap(from, to);
            }
        }
    }

    /// The plan being run.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The longest block [`process`](Self::process) accepts.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn check_frames(&self, frames: usize) -> Result<(), RuntimeError> {
        if frames > self.max_frames {
            return Err(RuntimeError::TooManyFrames {
                frames,
                max_frames: self.max_frames,
            });
        }
        Ok(())
    }

    /// Processes one block of `frames`: every step in order, outputs zeroed first.
    /// Stops at the first failing processor.
    pub fn process(&mut self, frames: usize) -> Result<(), RuntimeError> {
        self.check_frames(frames)?;
        let memory = Memory::of(&mut self.pool, &self.offsets, &mut self.silence);
        let buffers = self.plan.buffers();
        for (step, processor) in self.plan.steps().iter().zip(&mut self.processors) {
            // SAFETY: steps run one at a time, in an order that follows `after`.
            unsafe {
                run_step(
                    step,
                    buffers,
                    processor.as_mut(),
                    &memory,
                    frames,
                    &mut self.inputs,
                    &mut self.outputs,
                )?;
            }
        }
        Ok(())
    }

    /// Runs `total_frames` in blocks of at most [`max_frames`](Self::max_frames),
    /// calling `on_block` after each block with its length, to read the results with
    /// [`output`](Self::output).
    pub fn run(
        &mut self,
        total_frames: usize,
        mut on_block: impl FnMut(&Self, usize),
    ) -> Result<(), RuntimeError> {
        let mut done = 0;
        while done < total_frames {
            let frames = (total_frames - done).min(self.max_frames);
            self.process(frames)?;
            on_block(self, frames);
            done += frames;
        }
        Ok(())
    }

    /// [`run`](Self::run) with every block processed by
    /// [`process_parallel`](Self::process_parallel) on `threads` threads: one pass
    /// over a whole signal. Threads are started per block, so use big blocks.
    pub fn run_parallel(
        &mut self,
        total_frames: usize,
        threads: usize,
        mut on_block: impl FnMut(&Self, usize),
    ) -> Result<(), RuntimeError> {
        let mut done = 0;
        while done < total_frames {
            let frames = (total_frames - done).min(self.max_frames);
            self.process_parallel(frames, threads)?;
            on_block(self, frames);
            done += frames;
        }
        Ok(())
    }

    /// Processes one block of `frames` on up to `threads` threads (the calling one
    /// included): every step as soon as those in its [`Step::after`] are done, so
    /// independent branches run at the same time. Same result as
    /// [`process`](Self::process). Stops starting steps after the first failure and
    /// returns that error.
    pub fn process_parallel(&mut self, frames: usize, threads: usize) -> Result<(), RuntimeError> {
        self.check_frames(frames)?;
        let threads = threads.min(self.plan.steps().len());
        if threads <= 1 {
            return self.process(frames);
        }

        let steps = self.plan.steps();
        let mut followers = vec![Vec::new(); steps.len()];
        let mut waiting_for = Vec::with_capacity(steps.len());
        for (index, step) in steps.iter().enumerate() {
            waiting_for.push(step.after.len());
            for &earlier in &step.after {
                followers[earlier].push(index);
            }
        }
        let ready = (0..steps.len()).filter(|&i| steps[i].after.is_empty());
        let schedule = Mutex::new(Schedule {
            ready: ready.collect(),
            waiting_for,
            unfinished: steps.len(),
            error: None,
        });
        let wake = Condvar::new();
        let (most_inputs, most_outputs) = (self.inputs.capacity(), self.outputs.capacity());
        let shared = Shared {
            processors: self.processors.as_mut_ptr(),
            memory: Memory::of(&mut self.pool, &self.offsets, &mut self.silence),
        };
        let buffers = self.plan.buffers();
        let work = || {
            let mut inputs = Vec::with_capacity(most_inputs);
            let mut outputs = Vec::with_capacity(most_outputs);
            let mut guard = schedule.lock().expect("no panic while holding the lock");
            loop {
                if guard.unfinished == 0 || guard.error.is_some() {
                    wake.notify_all();
                    return;
                }
                let Some(index) = guard.ready.pop() else {
                    guard = wake.wait(guard).expect("no panic while holding the lock");
                    continue;
                };
                drop(guard);
                // SAFETY: `schedule` hands out each step once per block, so nothing
                // else uses its processor; the steps in `after` are finished, and
                // the plan promises that's all the order its buffers need. The mutex
                // orders the writes of those steps before this read.
                let result = unsafe {
                    let processor = &mut *shared.processor(index);
                    run_step(
                        &steps[index],
                        buffers,
                        processor.as_mut(),
                        shared.memory(),
                        frames,
                        &mut inputs,
                        &mut outputs,
                    )
                };
                guard = schedule.lock().expect("no panic while holding the lock");
                guard.unfinished -= 1;
                match result {
                    Ok(()) => {
                        for &follower in &followers[index] {
                            guard.waiting_for[follower] -= 1;
                            if guard.waiting_for[follower] == 0 {
                                guard.ready.push(follower);
                            }
                        }
                    }
                    Err(error) => guard.error = guard.error.take().or(Some(error)),
                }
                wake.notify_all();
            }
        };
        thread::scope(|scope| {
            for _ in 1..threads {
                scope.spawn(work);
            }
            work();
        });
        let schedule = schedule
            .into_inner()
            .expect("no panic while holding the lock");
        schedule.error.map_or(Ok(()), Err)
    }

    /// What `output` wrote in the last block of `frames`, if it's still there: only
    /// outputs feeding an output node stay valid after the block
    /// ([`Plan::buffer_of`]).
    pub fn output(&self, output: OutputRef, frames: usize) -> Option<&[f32]> {
        let buffer = self.plan.buffer_of(output)?;
        Some(self.buffer(buffer, frames))
    }

    /// What an output node got in the last block of `frames`.
    pub fn delivered(&self, output: &PlanOutput, frames: usize) -> &[f32] {
        let len = output.shape.buffer_len(frames.min(self.max_frames));
        match output.source {
            Input::Buffer(buffer) => self.buffer(buffer, frames),
            Input::Silence => &self.silence[..len],
        }
    }

    fn buffer(&self, buffer: BufferId, frames: usize) -> &[f32] {
        let shape = self.plan.buffers()[buffer.index()];
        let start = self.offsets[buffer.index()];
        let len = shape.buffer_len(frames.min(self.max_frames));
        &self.pool[start..start + len]
    }

    /// Sets parameter `index` (spec order) of node `node` for the next blocks.
    /// Returns whether it was set: `false` if the node isn't in the plan, there is
    /// no such parameter or its spec doesn't accept `value`.
    pub fn set_param(&mut self, node: NodeId, index: usize, value: ParamValue) -> bool {
        let Some(step) = self
            .plan
            .steps_mut()
            .iter_mut()
            .find(|step| step.node == node)
        else {
            return false;
        };
        if !step
            .spec
            .params
            .get(index)
            .is_some_and(|param| param.accepts(value))
        {
            return false;
        }
        step.params[index] = value;
        true
    }

    /// Forgets the signal history of every processor (playback jumped). A panicking
    /// `reset` is caught and leaves that processor as it is.
    pub fn reset(&mut self) {
        for processor in &mut self.processors {
            let _ = catch_unwind(AssertUnwindSafe(|| processor.reset()));
        }
    }

    /// The processors by node, e.g. to carry them over to an engine for a new plan.
    pub fn into_processors(self) -> HashMap<NodeId, Box<dyn NodeProcessor>> {
        let nodes = self.plan.steps().iter().map(|step| step.node);
        nodes.zip(self.processors).collect()
    }
}

/// What the threads of [`Engine::process_parallel`] share, behind a mutex.
struct Schedule {
    /// Steps with nothing left to wait for.
    ready: Vec<usize>,
    /// How many steps of its `after` each step still waits for.
    waiting_for: Vec<usize>,
    /// Steps not finished yet.
    unfinished: usize,
    error: Option<RuntimeError>,
}

/// The raw memory of one block, for steps that may run on several threads.
struct Memory<'a> {
    pool: *mut f32,
    offsets: &'a [usize],
    silence: *mut f32,
}

impl<'a> Memory<'a> {
    fn of(pool: &'a mut [f32], offsets: &'a [usize], silence: &'a mut [f32]) -> Self {
        Self {
            pool: pool.as_mut_ptr(),
            offsets,
            silence: silence.as_mut_ptr(),
        }
    }

    /// Start of `buffer` in the pool (inside it: the engine laid the offsets out).
    fn start(&self, buffer: BufferId) -> *mut f32 {
        self.pool.wrapping_add(self.offsets[buffer.index()])
    }
}

/// The processors and memory of an [`Engine`], for the threads of
/// [`Engine::process_parallel`].
struct Shared<'a> {
    processors: *mut Box<dyn NodeProcessor>,
    memory: Memory<'a>,
}

// SAFETY: threads use these pointers only as `process_parallel` says: a step's
// processor by the one thread running that step, buffers in the order the plan's
// `after` lists make safe (see `run_step`). Processors are `Send`.
unsafe impl Sync for Shared<'_> {}

impl Shared<'_> {
    /// The processor of step `index` (in bounds: steps and processors match).
    fn processor(&self, index: usize) -> *mut Box<dyn NodeProcessor> {
        self.processors.wrapping_add(index)
    }

    fn memory(&self) -> &Memory<'_> {
        &self.memory
    }
}

/// Runs `step`: its conversions, then its processor on `frames`, outputs zeroed
/// first. A panic in the processor comes back as an error.
///
/// # Safety
///
/// `memory` must hold the buffers of the plan `step` belongs to, laid out for at
/// least `frames`, and be valid for the call. Every step in `step.after` must be
/// finished and no step that uses a buffer of this step in a conflicting way may
/// be running (following [`Step::after`] guarantees that).
unsafe fn run_step(
    step: &Step,
    buffers: &[BufferShape],
    processor: &mut dyn NodeProcessor,
    memory: &Memory<'_>,
    frames: usize,
    inputs: &mut Vec<RawBuffer>,
    outputs: &mut Vec<RawBuffer>,
) -> Result<(), RuntimeError> {
    for conversion in &step.conversions {
        let from_shape = buffers[conversion.source.index()];
        let to_shape = buffers[conversion.buffer.index()];
        // SAFETY: two different buffers of the pool, each at most as long as laid
        // out for `max_frames`; by the contract nobody else writes either now, and
        // no reference into the pool is alive.
        let (from, to) = unsafe {
            (
                slice::from_raw_parts(
                    memory.start(conversion.source),
                    from_shape.buffer_len(frames),
                ),
                slice::from_raw_parts_mut(
                    memory.start(conversion.buffer),
                    to_shape.buffer_len(frames),
                ),
            )
        };
        convert_channels(from, to, to_shape.channel_len(frames));
    }

    inputs.clear();
    for (input, socket) in step.inputs.iter().zip(&step.spec.inputs) {
        let ptr = match *input {
            Input::Buffer(buffer) => memory.start(buffer),
            Input::Silence => memory.silence,
        };
        inputs.push(RawBuffer {
            ptr,
            len: socket.buffer_len(frames),
        });
    }
    outputs.clear();
    for (buffer, socket) in step.outputs.iter().zip(&step.spec.outputs) {
        let ptr = memory.start(*buffer);
        let len = socket.buffer_len(frames);
        // SAFETY: `len` ≤ the buffer's length for `max_frames`, and by the contract
        // this step is the only one using the buffer now.
        unsafe { slice::from_raw_parts_mut(ptr, len) }.fill(0.0);
        outputs.push(RawBuffer { ptr, len });
    }

    // SAFETY: one buffer per socket of the spec, each `buffer_len(frames)` long and
    // initialized. A step's outputs are distinct from each other, from `silence`
    // and from its inputs: the plan frees a buffer only after its last reader has
    // run, so a buffer read here can't be handed to this step's outputs.
    let mut io = unsafe { ProcessIo::from_raw(frames, &step.spec, &step.params, inputs, outputs) };
    match catch_unwind(AssertUnwindSafe(|| processor.process(&mut io))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(RuntimeError::Plugin {
            node: step.node,
            error,
        }),
        Err(_) => Err(RuntimeError::Panicked(step.node)),
    }
}

/// Fills `to` from `from`, both made of channels `channel_len` long, one of them
/// mono: copies a mono `from` into every channel of `to`, or averages the
/// channels of `from` into a mono `to`.
fn convert_channels(from: &[f32], to: &mut [f32], channel_len: usize) {
    if channel_len == 0 {
        return;
    }
    if from.len() == channel_len {
        for channel in to.chunks_exact_mut(channel_len) {
            channel.copy_from_slice(from);
        }
    } else {
        debug_assert_eq!(to.len(), channel_len, "one side is mono");
        to.fill(0.0);
        for channel in from.chunks_exact(channel_len) {
            for (out, sample) in to.iter_mut().zip(channel) {
                *out += sample;
            }
        }
        let scale = channel_len as f32 / from.len() as f32;
        for out in to {
            *out *= scale;
        }
    }
}

/// The nodes of `plan`, in step order: what [`Engine::successor`] takes as `previous`.
pub(crate) fn step_nodes(plan: &Plan) -> Vec<NodeId> {
    plan.steps().iter().map(|step| step.node).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use v_signal_nodes_core::{Graph, InputRef, Node, NodeKind, NodeSocket, NodeSpec, compile};
    use v_signal_nodes_plugin_host::PluginHost;
    use v_signal_nodes_plugin_std::{GAIN, SINK, SOURCE, StdPlugin};

    const FRAMES: usize = 64;

    /// A graph being built together with the processors of its nodes.
    struct Builder {
        host: PluginHost,
        graph: Graph,
        processors: HashMap<NodeId, Box<dyn NodeProcessor>>,
    }

    impl Builder {
        fn new() -> Self {
            let mut host = PluginHost::new();
            host.load(&StdPlugin).unwrap();
            Self {
                host,
                graph: Graph::new(),
                processors: HashMap::new(),
            }
        }

        fn add(&mut self, type_id: &str) -> NodeId {
            let id = self.graph.add(self.host.build_node(type_id).unwrap());
            self.processors
                .insert(id, self.host.create_processor(type_id).unwrap());
            id
        }

        fn connect(&mut self, from: NodeId, to: NodeId) {
            self.graph
                .connect(OutputRef::new(from, 0), InputRef::new(to, 0))
                .unwrap();
        }

        fn engine(self) -> Engine {
            let plan = compile(&self.graph).unwrap();
            Engine::new(plan, self.processors, 48_000.0, FRAMES).unwrap()
        }
    }

    /// Source → sink, and source → gain → sink, so both outputs stay readable; and
    /// the source and gain ids.
    fn branches() -> (Builder, NodeId, NodeId) {
        let mut builder = Builder::new();
        let source = builder.add(SOURCE);
        let gain = builder.add(GAIN);
        let direct = builder.add(SINK);
        let amplified = builder.add(SINK);
        builder.connect(source, direct);
        builder.connect(source, gain);
        builder.connect(gain, amplified);
        (builder, source, gain)
    }

    #[test]
    fn runs_branches_and_takes_parameter_changes() {
        let (builder, source, gain) = branches();
        let mut engine = builder.engine();

        engine.process(FRAMES).unwrap();
        let signal = engine
            .output(OutputRef::new(source, 0), FRAMES)
            .unwrap()
            .to_vec();
        let amplified = engine.output(OutputRef::new(gain, 0), FRAMES).unwrap();
        assert!(
            signal.iter().any(|&sample| sample != 0.0),
            "the source is silent"
        );
        assert_eq!(amplified, signal, "gain 1.0");

        let step = engine.plan().steps().iter().find(|step| step.node == gain);
        let factor = step.unwrap().spec.param_index("gain").unwrap();
        assert!(engine.set_param(gain, factor, ParamValue::Number(0.5)));
        assert!(!engine.set_param(gain, factor, ParamValue::Toggle(true)));
        engine.reset();
        engine.process(FRAMES).unwrap();
        let signal = engine.output(OutputRef::new(source, 0), FRAMES).unwrap();
        let amplified = engine.output(OutputRef::new(gain, 0), FRAMES).unwrap();
        for (out, sample) in amplified.iter().zip(signal) {
            assert_eq!(*out, sample * 0.5);
        }
    }

    #[test]
    fn output_nodes_get_their_signal_converted() {
        let mut builder = Builder::new();
        let source = builder.add(SOURCE);
        let sink = builder.add(SINK);
        builder.connect(source, sink);
        let mut engine = builder.engine();

        engine.process(FRAMES).unwrap();

        let output = engine.plan().outputs()[0];
        assert_eq!(output.node, sink);
        let delivered = engine.delivered(&output, FRAMES);
        assert_eq!(delivered.len(), FRAMES * 2, "stereo");
        let (left, right) = delivered.split_at(FRAMES);
        assert_eq!(left, right);
        assert_eq!(
            left,
            engine.output(OutputRef::new(source, 0), FRAMES).unwrap()
        );
    }

    #[test]
    fn an_unconnected_output_node_gets_silence() {
        let mut builder = Builder::new();
        builder.add(SINK);
        let mut engine = builder.engine();

        engine.process(FRAMES).unwrap();

        let output = engine.plan().outputs()[0];
        let delivered = engine.delivered(&output, FRAMES);
        assert_eq!(delivered, vec![0.0; FRAMES * 2]);
    }

    #[test]
    fn channels_are_converted_between_mono_and_stereo() {
        let mut stereo = [0.0; 4];
        convert_channels(&[1.0, 2.0], &mut stereo, 2);
        assert_eq!(stereo, [1.0, 2.0, 1.0, 2.0]);

        let mut mono = [9.0; 2];
        convert_channels(&[1.0, 2.0, 3.0, 6.0], &mut mono, 2);
        assert_eq!(mono, [2.0, 4.0]);
    }

    #[test]
    fn run_splits_into_blocks() {
        let (builder, ..) = branches();
        let mut engine = builder.engine();
        let mut blocks = Vec::new();

        engine
            .run(FRAMES * 2 + 10, |_, frames| blocks.push(frames))
            .unwrap();

        assert_eq!(blocks, [FRAMES, FRAMES, 10]);
        assert!(matches!(
            engine.process(FRAMES + 1),
            Err(RuntimeError::TooManyFrames { .. })
        ));
    }

    #[test]
    fn every_step_needs_a_processor() {
        let (mut builder, _, gain) = branches();
        builder.processors.remove(&gain);
        let plan = compile(&builder.graph).unwrap();

        assert!(matches!(
            Engine::new(plan, builder.processors, 48_000.0, FRAMES),
            Err(RuntimeError::MissingProcessor(node)) if node == gain
        ));
    }

    #[test]
    fn processors_can_be_taken_back() {
        let (builder, source, gain) = branches();
        let engine = builder.engine();

        let back: HashSet<_> = engine.into_processors().into_keys().collect();
        assert!(back.contains(&source) && back.contains(&gain));
        assert_eq!(back.len(), 4);
    }

    /// A test node: counts its `process` calls in `calls` and writes the count.
    struct Counter {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for Counter {
        fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
            let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            io.output(0).fill(calls as f32);
            Ok(())
        }
    }

    /// A generator spec with one mono output, and an output node with a mono input.
    fn generator_and_sink() -> (Arc<NodeSpec>, Arc<NodeSpec>) {
        (
            Arc::new(
                NodeSpec::new("test.gen", "Gen", NodeKind::Generator)
                    .output(NodeSocket::signal_out("out")),
            ),
            Arc::new(
                NodeSpec::new("test.sink", "Sink", NodeKind::Output)
                    .input(NodeSocket::signal_in("in")),
            ),
        )
    }

    struct NoOp;

    impl NodeProcessor for NoOp {
        fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
            Ok(())
        }
    }

    #[test]
    fn a_successor_keeps_the_state_of_shared_nodes() {
        let (generator, sink) = generator_and_sink();
        let mut graph = Graph::new();
        let kept = graph.add(Node::new(Arc::clone(&generator)));
        let first_sink = graph.add(Node::new(Arc::clone(&sink)));
        graph
            .connect(OutputRef::new(kept, 0), InputRef::new(first_sink, 0))
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut processors: HashMap<NodeId, Box<dyn NodeProcessor>> = HashMap::new();
        processors.insert(
            kept,
            Box::new(Counter {
                calls: Arc::clone(&calls),
            }),
        );
        processors.insert(first_sink, Box::new(NoOp));
        let mut old = Engine::new(compile(&graph).unwrap(), processors, 48_000.0, FRAMES).unwrap();
        old.process(FRAMES).unwrap();
        old.process(FRAMES).unwrap();

        // A new sink joins; the generator must keep counting from where it was.
        let second_sink = graph.add(Node::new(sink));
        graph
            .connect(OutputRef::new(kept, 0), InputRef::new(second_sink, 0))
            .unwrap();
        let plan = compile(&graph).unwrap();
        let previous = step_nodes(old.plan());
        let fresh = processors_for(&plan, &previous, |_| {
            Ok::<_, String>(Box::new(NoOp) as Box<dyn NodeProcessor>)
        })
        .unwrap();
        assert_eq!(fresh.len(), 1, "only the new node needs a processor");
        let mut new = Engine::successor(plan, &previous, fresh, 48_000.0, FRAMES).unwrap();
        new.adopt(&mut old);
        drop(old);

        new.process(FRAMES).unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let output = new.output(OutputRef::new(kept, 0), FRAMES).unwrap();
        assert_eq!(output[0], 3.0);
    }

    #[test]
    fn a_panicking_processor_is_an_error() {
        struct Panics;

        impl NodeProcessor for Panics {
            fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
                panic!("test panic in a processor");
            }
        }

        let (generator, sink) = generator_and_sink();
        let mut graph = Graph::new();
        let bad = graph.add(Node::new(generator));
        let end = graph.add(Node::new(sink));
        graph
            .connect(OutputRef::new(bad, 0), InputRef::new(end, 0))
            .unwrap();
        let mut processors: HashMap<NodeId, Box<dyn NodeProcessor>> = HashMap::new();
        processors.insert(bad, Box::new(Panics));
        processors.insert(end, Box::new(NoOp));
        let plan = compile(&graph).unwrap();
        let mut engine = Engine::new(plan, processors, 48_000.0, FRAMES).unwrap();

        assert!(matches!(
            engine.process(FRAMES),
            Err(RuntimeError::Panicked(node)) if node == bad
        ));
        assert!(matches!(
            engine.process_parallel(FRAMES, 2),
            Err(RuntimeError::Panicked(node)) if node == bad
        ));
    }

    /// A chain-and-fan graph of std nodes that needs conversions and buffer reuse.
    fn busy_graph() -> Builder {
        let mut builder = Builder::new();
        let first = builder.add(SOURCE);
        let second = builder.add(SOURCE);
        for source in [first, second] {
            let mut last = source;
            for _ in 0..4 {
                let gain = builder.add(GAIN);
                builder.connect(last, gain);
                last = gain;
            }
            let sink = builder.add(SINK);
            builder.connect(last, sink);
            let direct = builder.add(SINK);
            builder.connect(source, direct);
        }
        builder
    }

    #[test]
    fn parallel_processing_gives_the_same_result() {
        let mut one_by_one = busy_graph().engine();
        let mut parallel = busy_graph().engine();

        for _ in 0..50 {
            one_by_one.process(FRAMES).unwrap();
            parallel.process_parallel(FRAMES, 4).unwrap();
            let outputs = one_by_one.plan().outputs().iter();
            for (a, b) in outputs.zip(parallel.plan().outputs()) {
                assert_eq!(
                    one_by_one.delivered(a, FRAMES),
                    parallel.delivered(b, FRAMES)
                );
            }
        }
    }

    #[test]
    fn independent_branches_run_at_the_same_time() {
        /// Waits until `running` counts both branches inside `process` at once;
        /// fails if that doesn't happen, as it never would if they ran in turn.
        struct Meets {
            running: Arc<AtomicUsize>,
        }

        impl NodeProcessor for Meets {
            fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
                self.running.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(10);
                while self.running.load(Ordering::SeqCst) < 2 {
                    if Instant::now() > deadline {
                        return Err(PluginError::new("the other branch didn't run"));
                    }
                    thread::yield_now();
                }
                Ok(())
            }
        }

        let (generator, sink) = generator_and_sink();
        let mut graph = Graph::new();
        let running = Arc::new(AtomicUsize::new(0));
        let mut processors: HashMap<NodeId, Box<dyn NodeProcessor>> = HashMap::new();
        for _ in 0..2 {
            let start = graph.add(Node::new(Arc::clone(&generator)));
            let end = graph.add(Node::new(Arc::clone(&sink)));
            graph
                .connect(OutputRef::new(start, 0), InputRef::new(end, 0))
                .unwrap();
            processors.insert(
                start,
                Box::new(Meets {
                    running: Arc::clone(&running),
                }),
            );
            processors.insert(end, Box::new(NoOp));
        }
        let plan = compile(&graph).unwrap();
        let mut engine = Engine::new(plan, processors, 48_000.0, FRAMES).unwrap();

        engine.process_parallel(FRAMES, 2).unwrap();
    }

    #[test]
    fn run_parallel_covers_every_frame() {
        let mut engine = busy_graph().engine();
        let mut total = 0;

        engine
            .run_parallel(FRAMES * 3 + 1, 3, |_, frames| total += frames)
            .unwrap();

        assert_eq!(total, FRAMES * 3 + 1);
    }
}

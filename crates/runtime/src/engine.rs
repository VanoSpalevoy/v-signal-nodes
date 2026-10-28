//! [`Engine`]: runs a [`Plan`] block by block.
//!
//! All buffers live in one pool allocated up front for `max_frames`; a block of
//! `frames` uses the first [`NodeSocket::buffer_len`]`(frames)` samples of each.
//! [`process`](Engine::process) only walks the steps, points each processor at its
//! buffers and calls it: no allocation, no locks (apart from the error path, see
//! [`RuntimeError`]).
//!
//! TODO(P1-1):
//! - processors keyed by `NodeId` that survive a new plan (keep phases, delays);
//! - parallel independent branches for the one-pass mode;
//! - steps should carry their own sockets once nodes' settings can change them
//!   (`core::Node::sockets`); for now the spec's are used.

use std::collections::HashMap;
use std::slice;

use v_signal_nodes_core::{BufferId, Input, NodeId, NodeSocket, OutputRef, ParamValue, Plan};
use v_signal_nodes_plugin_api::{NodeProcessor, ProcessIo, RawBuffer};

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
}

// SAFETY: the only non-`Send` fields are the `RawBuffer` scratch vectors. Their
// pointers are rebuilt from `pool`/`silence` at the start of every step in
// `process` and only dereferenced there, by the processor called within the same
// `&mut self` borrow; a stale pointer left behind is never read. Everything else
// is `Send` (`NodeProcessor: Send`).
unsafe impl Send for Engine {}

impl Engine {
    /// Prepares `plan` for blocks of at most `max_frames` at `sample_rate`: takes
    /// the processor of every step's node from `processors` (the rest are dropped),
    /// allocates the buffers and calls `prepare` on each processor.
    pub fn new(
        plan: Plan,
        mut processors: HashMap<NodeId, Box<dyn NodeProcessor>>,
        sample_rate: f32,
        max_frames: usize,
    ) -> Result<Self, RuntimeError> {
        let mut ordered = Vec::with_capacity(plan.steps().len());
        for step in plan.steps() {
            let processor = processors
                .remove(&step.node)
                .ok_or(RuntimeError::MissingProcessor(step.node))?;
            ordered.push(processor);
        }

        let mut offsets = Vec::with_capacity(plan.buffers().len());
        let mut pool_len = 0;
        for socket in plan.buffers() {
            offsets.push(pool_len);
            pool_len += socket.buffer_len(max_frames);
        }
        let inputs = plan.steps().iter().flat_map(|step| &step.spec.inputs);
        let silence_len = inputs
            .clone()
            .map(|socket| socket.buffer_len(max_frames))
            .max()
            .unwrap_or(0);
        let most = |count: fn(&v_signal_nodes_core::Step) -> usize| {
            plan.steps().iter().map(count).max().unwrap_or(0)
        };
        let (max_inputs, max_outputs) = (most(|s| s.inputs.len()), most(|s| s.outputs.len()));

        for (step, processor) in plan.steps().iter().zip(&mut ordered) {
            processor
                .prepare(sample_rate, max_frames)
                .map_err(|error| RuntimeError::Plugin {
                    node: step.node,
                    error,
                })?;
        }
        Ok(Self {
            plan,
            processors: ordered,
            pool: vec![0.0; pool_len],
            offsets,
            silence: vec![0.0; silence_len],
            max_frames,
            inputs: Vec::with_capacity(max_inputs),
            outputs: Vec::with_capacity(max_outputs),
        })
    }

    /// The plan being run.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The longest block [`process`](Self::process) accepts.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Processes one block of `frames`: every step in order, outputs zeroed first.
    /// Stops at the first failing processor.
    pub fn process(&mut self, frames: usize) -> Result<(), RuntimeError> {
        if frames > self.max_frames {
            return Err(RuntimeError::TooManyFrames {
                frames,
                max_frames: self.max_frames,
            });
        }
        let pool = self.pool.as_mut_ptr();
        let offsets = &self.offsets;
        // Start of `buffer` in `pool` (inside it: `new` laid the offsets out).
        let start = |buffer: BufferId| pool.wrapping_add(offsets[buffer.index()]);
        let silence = self.silence.as_mut_ptr();
        for (step, processor) in self.plan.steps().iter().zip(&mut self.processors) {
            self.inputs.clear();
            for (input, socket) in step.inputs.iter().zip(&step.spec.inputs) {
                let len = socket.buffer_len(frames);
                let ptr = match *input {
                    Input::Buffer(buffer) => start(buffer),
                    Input::Silence => silence,
                    Input::Converted { source, buffer } => {
                        let from_socket = &self.plan.buffers()[source.index()];
                        let (from_ptr, to_ptr) = (start(source), start(buffer));
                        // SAFETY: two different buffers of `pool`, each at most as
                        // long as laid out for `max_frames`; no references into
                        // `pool` are alive.
                        let (from, to) = unsafe {
                            (
                                slice::from_raw_parts(from_ptr, from_socket.buffer_len(frames)),
                                slice::from_raw_parts_mut(to_ptr, len),
                            )
                        };
                        convert_channels(from, to, socket.channel_len(frames));
                        to_ptr
                    }
                };
                self.inputs.push(RawBuffer { ptr, len });
            }
            self.outputs.clear();
            for (buffer, socket) in step.outputs.iter().zip(&step.spec.outputs) {
                let ptr = start(*buffer);
                let len = socket.buffer_len(frames);
                // SAFETY: `len` ≤ the buffer's length for `max_frames`, and no other
                // reference into `pool` is alive.
                unsafe { slice::from_raw_parts_mut(ptr, len) }.fill(0.0);
                self.outputs.push(RawBuffer { ptr, len });
            }
            // SAFETY: one buffer per socket of the spec, each `buffer_len(frames)`
            // long and initialized. A step's outputs are its own buffers: distinct
            // from each other, from `silence` and from its inputs, which are other
            // steps' outputs (the graph has no cycles, so no self-feeding).
            let mut io = unsafe {
                ProcessIo::from_raw(
                    frames,
                    &step.spec,
                    &step.params,
                    &self.inputs,
                    &self.outputs,
                )
            };
            processor
                .process(&mut io)
                .map_err(|error| RuntimeError::Plugin {
                    node: step.node,
                    error,
                })?;
        }
        Ok(())
    }

    /// Runs `total_frames` in blocks of at most [`max_frames`](Self::max_frames)
    /// (offline / one pass), calling `on_block` after each block with its length,
    /// to read the results with [`output`](Self::output).
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

    /// What `output` wrote in the last block of `frames`, if its node is in the
    /// plan.
    pub fn output(&self, output: OutputRef, frames: usize) -> Option<&[f32]> {
        let buffer = self.plan.buffer_of(output)?;
        let socket: &NodeSocket = &self.plan.buffers()[buffer.index()];
        let start = self.offsets[buffer.index()];
        let len = socket.buffer_len(frames.min(self.max_frames));
        Some(&self.pool[start..start + len])
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

    /// Forgets the signal history of every processor (playback jumped).
    pub fn reset(&mut self) {
        for processor in &mut self.processors {
            processor.reset();
        }
    }

    /// The processors by node, e.g. to carry them over to an engine for a new plan.
    pub fn into_processors(self) -> HashMap<NodeId, Box<dyn NodeProcessor>> {
        let nodes = self.plan.steps().iter().map(|step| step.node);
        nodes.zip(self.processors).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::{Graph, InputRef, compile};
    use v_signal_nodes_plugin_host::PluginHost;
    use v_signal_nodes_plugin_std::{GAIN, SINK, SOURCE, StdPlugin};

    const FRAMES: usize = 64;

    /// Source → gain → sink, compiled, with the processors; and the three ids.
    fn chain() -> (Plan, HashMap<NodeId, Box<dyn NodeProcessor>>, [NodeId; 3]) {
        let mut host = PluginHost::new();
        host.load(&StdPlugin).unwrap();
        let mut graph = Graph::new();
        let mut processors = HashMap::new();
        let mut add = |type_id| {
            let built = host.build_node(type_id).unwrap();
            let id = graph.add(built.node);
            processors.insert(id, built.processor);
            id
        };
        let ids = [add(SOURCE), add(GAIN), add(SINK)];
        for pair in ids.windows(2) {
            graph
                .connect(OutputRef::new(pair[0], 0), InputRef::new(pair[1], 0))
                .unwrap();
        }
        (compile(&graph).unwrap(), processors, ids)
    }

    #[test]
    fn runs_a_chain_and_takes_parameter_changes() {
        let (plan, processors, [source, gain, _]) = chain();
        let mut engine = Engine::new(plan, processors, 48_000.0, FRAMES).unwrap();

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

        let factor = engine.plan().steps()[1].spec.param_index("gain").unwrap();
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
        let (plan, processors, _) = chain();
        let mut engine = Engine::new(plan, processors, 48_000.0, FRAMES).unwrap();
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
        let (plan, mut processors, [_, gain, _]) = chain();
        processors.remove(&gain);

        assert!(matches!(
            Engine::new(plan, processors, 48_000.0, FRAMES),
            Err(RuntimeError::MissingProcessor(node)) if node == gain
        ));
    }

    #[test]
    fn processors_can_be_taken_back() {
        let (plan, processors, ids) = chain();
        let engine = Engine::new(plan, processors, 48_000.0, FRAMES).unwrap();

        let mut back: Vec<_> = engine.into_processors().into_keys().collect();
        back.sort();
        assert_eq!(back, ids);
    }
}

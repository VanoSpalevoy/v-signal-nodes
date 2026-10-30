//! Real-time processing: an [`Engine`] on the device's thread, driven by messages
//! from the thread that owns the graph.
//!
//! ```text
//!  control thread (Controller)                 real-time thread (Renderer)
//!  ───────────────────────────                 ───────────────────────────
//!  compile(&graph) → Plan                      per device block:
//!  create new nodes' processors, prepare         drain commands (never blocks)
//!  ── Command::Replace(engine) ──────────────►   new.adopt(&mut old): state moves over
//!  ── Command::SetParam { .. } ─────────────►    engine.process(frames), up to max_frames
//!  ◄── Event::Replaced(old engine) ───────────   sum output nodes into the device buffer
//!  drops it here
//! ```
//!
//! Rules on the real-time thread: no allocation, no locks, no panics, no dropping of
//! anything big. So the queues are lock-free and fixed in size ([`rtrb`]), a processor
//! is created and prepared before it gets there, and a replaced engine travels back
//! to be dropped. The exception is failing: a processor's error message is a `String`
//! and a caught panic allocates, but after that the [`Renderer`] outputs silence
//! until the next engine, so that happens once.
//!
//! Output nodes: every input of every output node is summed into the device's
//! channels ([`mix_into`]); the sum is limited to `-1.0..=1.0`, what a device takes.
//! Control-rate inputs aren't delivered, they have no samples.
//!
//! The device backend lives outside this crate, as a driver calling
//! [`Renderer::render`], so all of this is testable offline.

use std::fmt::Display;

use rtrb::{Consumer, Producer, RingBuffer};
use v_signal_nodes_core::{NodeId, ParamValue, Plan, SignalRate, Step};
use v_signal_nodes_plugin_api::NodeProcessor;

use crate::engine::step_nodes;
use crate::{Engine, RuntimeError, processors_for};

/// Messages waiting for the real-time thread, at most.
const COMMAND_CAPACITY: usize = 1024;
/// Engines on their way to or from the real-time thread, at most: the
/// [`Controller`] refuses more, so the way back can't fill up.
const MAX_REPLACES_IN_FLIGHT: usize = 32;
/// Room for what a replace can send back: the old engine and one failure.
const EVENT_CAPACITY: usize = MAX_REPLACES_IN_FLIGHT * 2 + 2;

/// A message from the control thread to the real-time thread.
pub enum Command {
    /// Change one parameter (spec order) of one node from the next block on.
    SetParam {
        node: NodeId,
        index: usize,
        value: ParamValue,
    },
    /// Forget the signal history (playback jumped).
    Reset,
    /// Run a new, already prepared engine (a recompiled graph) from the next block.
    Replace(Box<Engine>),
    /// Stop running anything: the output is silent until the next `Replace`.
    Clear,
}

/// A message from the real-time thread back to the control thread.
pub enum Event {
    /// A `Replace` or `Clear` took effect; the engine it replaced, if any, to drop.
    Replaced(Option<Box<Engine>>),
    /// The running engine failed; the output is silent until the next `Replace`.
    Failed(RuntimeError),
}

/// Creates the two ends of a real-time run at `sample_rate`, in blocks of at most
/// `max_frames`: the [`Controller`] stays on the control thread, the [`Renderer`]
/// goes to the device's.
pub fn channel(sample_rate: f32, max_frames: usize) -> (Controller, Renderer) {
    let (commands, command_queue) = RingBuffer::new(COMMAND_CAPACITY);
    let (event_queue, events) = RingBuffer::new(EVENT_CAPACITY);
    let controller = Controller {
        commands,
        events,
        sample_rate,
        max_frames,
        running: Vec::new(),
        replaces_in_flight: 0,
    };
    let renderer = Renderer {
        commands: command_queue,
        events: event_queue,
        engine: None,
        failed: false,
    };
    (controller, renderer)
}

/// The control thread's end: builds engines and sends messages.
pub struct Controller {
    commands: Producer<Command>,
    events: Consumer<Event>,
    sample_rate: f32,
    max_frames: usize,
    /// The nodes of the last engine sent, in step order: what the next one replaces.
    running: Vec<NodeId>,
    /// `Replace`s and `Clear`s sent and not answered yet.
    replaces_in_flight: usize,
}

impl Controller {
    /// Runs `plan` from the next block on: processors of nodes already running move
    /// over with their state, `make` creates the others, and they are prepared here.
    /// Fails, with the old plan still running, if a processor can't be created or
    /// prepared. [`RuntimeError::Busy`] means nothing was sent: try again after
    /// [`poll`](Self::poll).
    pub fn run_plan<E: Display>(
        &mut self,
        plan: Plan,
        make: impl FnMut(&Step) -> Result<Box<dyn NodeProcessor>, E>,
    ) -> Result<(), RuntimeError> {
        if !self.can_replace() {
            return Err(RuntimeError::Busy);
        }
        let fresh = processors_for(&plan, &self.running, make)?;
        let nodes = step_nodes(&plan);
        let engine = Engine::successor(
            plan,
            &self.running,
            fresh,
            self.sample_rate,
            self.max_frames,
        )?;
        self.send(Command::Replace(Box::new(engine)))
            .map_err(|_| RuntimeError::Busy)?;
        self.running = nodes;
        self.replaces_in_flight += 1;
        Ok(())
    }

    /// Stops running anything; the output goes silent and every processor is dropped.
    pub fn clear(&mut self) -> Result<(), RuntimeError> {
        if !self.can_replace() {
            return Err(RuntimeError::Busy);
        }
        self.send(Command::Clear).map_err(|_| RuntimeError::Busy)?;
        self.running.clear();
        self.replaces_in_flight += 1;
        Ok(())
    }

    /// Whether anything is running or on its way to run.
    pub fn is_running(&self) -> bool {
        !self.running.is_empty()
    }

    /// Sets parameter `index` (spec order) of `node` from the next block on. Returns
    /// whether the message was sent; the engine ignores a value the spec doesn't
    /// accept.
    pub fn set_param(&mut self, node: NodeId, index: usize, value: ParamValue) -> bool {
        self.send(Command::SetParam { node, index, value }).is_ok()
    }

    /// Makes every processor forget its signal history. Returns whether the message
    /// was sent.
    pub fn reset(&mut self) -> bool {
        self.send(Command::Reset).is_ok()
    }

    /// Handles what came back from the real-time thread and returns the failure, if
    /// any: replaced engines are dropped here. Call it regularly.
    pub fn poll(&mut self) -> Option<RuntimeError> {
        let mut failure = None;
        while let Ok(event) = self.events.pop() {
            match event {
                Event::Replaced(_) => self.replaces_in_flight -= 1,
                Event::Failed(error) => failure = Some(error),
            }
        }
        failure
    }

    fn can_replace(&self) -> bool {
        self.replaces_in_flight < MAX_REPLACES_IN_FLIGHT
    }

    fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands.push(command).map_err(|err| match err {
            rtrb::PushError::Full(command) => command,
        })
    }
}

/// The real-time thread's end: applies messages and renders blocks. Nothing it does
/// on the way to a device allocates, locks or blocks while the engine works.
pub struct Renderer {
    commands: Consumer<Command>,
    events: Producer<Event>,
    engine: Option<Box<Engine>>,
    /// The engine failed: don't run it again until it's replaced.
    failed: bool,
}

impl Renderer {
    /// Fills `device`, interleaved with `channels` channels per frame: applies waiting
    /// messages, then processes as many blocks as it takes and mixes what the output
    /// nodes got. Silent when nothing runs or the engine failed.
    pub fn render(&mut self, device: &mut [f32], channels: usize) {
        device.fill(0.0);
        self.apply_commands();
        if channels == 0 {
            return;
        }
        let Some(mut engine) = self.engine.take() else {
            return;
        };
        let block_len = engine.max_frames() * channels;
        if !self.failed && block_len > 0 {
            for block in device.chunks_mut(block_len) {
                let frames = block.len() / channels;
                if let Err(error) = engine.process(frames) {
                    self.failed = true;
                    self.send(Event::Failed(error));
                    block.fill(0.0);
                    break;
                }
                for output in engine.plan().outputs() {
                    if output.shape.rate == SignalRate::Sample {
                        mix_into(
                            block,
                            channels,
                            engine.delivered(output, frames),
                            usize::from(output.shape.channels),
                        );
                    }
                }
            }
        }
        self.engine = Some(engine);
        for sample in device {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    fn apply_commands(&mut self) {
        while let Ok(command) = self.commands.pop() {
            match command {
                Command::SetParam { node, index, value } => {
                    if let Some(engine) = &mut self.engine {
                        engine.set_param(node, index, value);
                    }
                }
                Command::Reset => {
                    if let Some(engine) = &mut self.engine {
                        engine.reset();
                    }
                }
                Command::Replace(mut new) => {
                    if let Some(old) = &mut self.engine {
                        new.adopt(old);
                    }
                    self.failed = false;
                    let old = self.engine.replace(new);
                    self.send(Event::Replaced(old));
                }
                Command::Clear => {
                    self.failed = false;
                    let old = self.engine.take();
                    self.send(Event::Replaced(old));
                }
            }
        }
    }

    /// Sends `event` back. The queue has room by construction; if it ever doesn't,
    /// the event leaks instead of being dropped here.
    fn send(&mut self, event: Event) {
        if let Err(rtrb::PushError::Full(event)) = self.events.push(event) {
            std::mem::forget(event);
        }
    }
}

/// Adds `signal` (a socket's buffer: `signal_channels` channels one after another) to
/// the interleaved `device`, `device_channels` per frame: mono goes to every device
/// channel, channel `c` to device channel `c`; on a mono device all channels are
/// averaged. Device channels without a signal channel get nothing, and signal
/// channels without a device channel are left out.
pub fn mix_into(
    device: &mut [f32],
    device_channels: usize,
    signal: &[f32],
    signal_channels: usize,
) {
    if device_channels == 0 || signal_channels == 0 {
        return;
    }
    let frames = device.len() / device_channels;
    if frames == 0 || signal.len() < frames * signal_channels {
        return;
    }
    if signal_channels == 1 {
        for (frame, &sample) in device.chunks_exact_mut(device_channels).zip(signal) {
            for out in frame {
                *out += sample;
            }
        }
    } else if device_channels == 1 {
        let scale = 1.0 / signal_channels as f32;
        for channel in signal.chunks_exact(frames).take(signal_channels) {
            for (out, sample) in device.iter_mut().zip(channel) {
                *out += sample * scale;
            }
        }
    } else {
        let channels = signal.chunks_exact(frames).take(device_channels);
        for (index, channel) in channels.enumerate() {
            for (frame, sample) in device.chunks_exact_mut(device_channels).zip(channel) {
                frame[index] += sample;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::{Graph, InputRef, OutputRef, compile};
    use v_signal_nodes_plugin_host::PluginHost;
    use v_signal_nodes_plugin_std::{GAIN, SINK, SOURCE, StdPlugin};

    const MAX_FRAMES: usize = 64;

    struct Run {
        host: PluginHost,
        graph: Graph,
        controller: Controller,
        renderer: Renderer,
    }

    impl Run {
        fn new() -> Self {
            let mut host = PluginHost::new();
            host.load(&StdPlugin).unwrap();
            let (controller, renderer) = channel(48_000.0, MAX_FRAMES);
            Self {
                host,
                graph: Graph::new(),
                controller,
                renderer,
            }
        }

        fn add(&mut self, type_id: &str) -> NodeId {
            self.graph.add(self.host.build_node(type_id).unwrap())
        }

        fn connect(&mut self, from: NodeId, to: NodeId) {
            self.graph
                .connect(OutputRef::new(from, 0), InputRef::new(to, 0))
                .unwrap();
        }

        /// Compiles the graph and sends the plan.
        fn send_plan(&mut self) -> Result<(), RuntimeError> {
            let plan = compile(&self.graph).unwrap();
            let host = &self.host;
            self.controller
                .run_plan(plan, |step| host.create_processor(&step.spec.type_id))
        }

        /// Renders `frames` stereo frames.
        fn render(&mut self, frames: usize) -> Vec<f32> {
            let mut device = vec![9.0; frames * 2];
            self.renderer.render(&mut device, 2);
            device
        }
    }

    #[test]
    fn silent_until_a_plan_arrives() {
        let mut run = Run::new();
        assert_eq!(run.render(4), [0.0; 8]);
        assert!(!run.controller.is_running());
    }

    #[test]
    fn plays_the_output_node_across_device_blocks() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        run.send_plan().unwrap();

        // Longer than one engine block, so it takes several.
        let device = run.render(MAX_FRAMES * 2 + 5);

        assert!(device.iter().any(|&sample| sample != 0.0));
        for [left, right] in device.as_chunks::<2>().0 {
            assert_eq!(left, right, "mono goes to both channels");
        }
        assert!(run.controller.is_running());
    }

    #[test]
    fn a_new_plan_keeps_the_phase_of_running_nodes() {
        // Stopping a sine mid-period and restarting from a fresh processor would jump;
        // with the state kept, the next samples continue the same wave.
        let mut continued = Run::new();
        let source = continued.add(SOURCE);
        let sink = continued.add(SINK);
        continued.connect(source, sink);
        continued.send_plan().unwrap();
        let mut uninterrupted = Run::new();
        let source2 = uninterrupted.add(SOURCE);
        let sink2 = uninterrupted.add(SINK);
        uninterrupted.connect(source2, sink2);
        uninterrupted.send_plan().unwrap();
        let first = continued.render(MAX_FRAMES);
        assert_eq!(first, uninterrupted.render(MAX_FRAMES));

        // A node joins that adds nothing to the signal path of the first sink.
        let other = continued.add(SOURCE);
        let extra = continued.add(GAIN);
        continued.connect(other, extra);
        continued.send_plan().unwrap();

        assert_eq!(
            continued.render(MAX_FRAMES),
            uninterrupted.render(MAX_FRAMES),
            "the source went on from its phase"
        );
    }

    #[test]
    fn replaced_engines_come_back_to_be_dropped() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        run.send_plan().unwrap();
        run.send_plan().unwrap();
        run.render(1);

        assert!(run.controller.poll().is_none());
        assert_eq!(run.controller.replaces_in_flight, 0);
    }

    #[test]
    fn too_many_unanswered_replaces_are_refused_and_recover() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        for _ in 0..MAX_REPLACES_IN_FLIGHT {
            run.send_plan().unwrap();
        }

        assert!(matches!(run.send_plan(), Err(RuntimeError::Busy)));

        run.render(1);
        run.controller.poll();
        run.send_plan().unwrap();
    }

    #[test]
    fn parameter_messages_reach_the_engine() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let gain = run.add(GAIN);
        let sink = run.add(SINK);
        run.connect(source, gain);
        run.connect(gain, sink);
        run.send_plan().unwrap();
        run.render(1);
        let factor = run
            .graph
            .node(gain)
            .unwrap()
            .spec()
            .param_index("gain")
            .unwrap();

        assert!(
            run.controller
                .set_param(gain, factor, ParamValue::Number(0.0))
        );

        assert_eq!(run.render(8), [0.0; 16]);
    }

    #[test]
    fn reset_restarts_the_signal() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        run.send_plan().unwrap();
        let first = run.render(8);
        assert_ne!(first, run.render(8));

        assert!(run.controller.reset());

        assert_eq!(first, run.render(8));
    }

    #[test]
    fn clear_goes_silent() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        run.send_plan().unwrap();
        run.render(8);

        run.controller.clear().unwrap();

        assert_eq!(run.render(8), [0.0; 16]);
        assert!(!run.controller.is_running());
        assert!(run.controller.poll().is_none());
        assert_eq!(run.controller.replaces_in_flight, 0);
    }

    #[test]
    fn a_failure_is_reported_once_and_goes_silent() {
        use v_signal_nodes_plugin_api::{PluginError, ProcessIo};

        struct Fails;

        impl NodeProcessor for Fails {
            fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
                Err(PluginError::new("test failure"))
            }
        }

        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        let plan = compile(&run.graph).unwrap();
        run.controller
            .run_plan(plan, |_| {
                Ok::<_, String>(Box::new(Fails) as Box<dyn NodeProcessor>)
            })
            .unwrap();

        assert_eq!(run.render(8), [0.0; 16]);
        assert!(matches!(
            run.controller.poll(),
            Some(RuntimeError::Plugin { .. })
        ));
        assert_eq!(run.render(8), [0.0; 16]);
        assert!(run.controller.poll().is_none(), "not reported again");
    }

    #[test]
    fn a_failed_build_leaves_the_old_plan_running() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        let sink = run.add(SINK);
        run.connect(source, sink);
        run.send_plan().unwrap();
        run.render(8);

        let extra = run.add(SINK);
        run.connect(source, extra);
        let plan = compile(&run.graph).unwrap();
        let result = run
            .controller
            .run_plan(plan, |_| Err::<Box<dyn NodeProcessor>, _>("no such plugin"));

        assert!(matches!(
            result,
            Err(RuntimeError::CreateProcessor { node, .. }) if node == extra
        ));
        assert!(run.render(8).iter().any(|&sample| sample != 0.0));
    }

    #[test]
    fn signals_are_mixed_into_device_channels() {
        // Mono into every channel, added to what's there.
        let mut device = [1.0, 1.0, 1.0, 1.0];
        mix_into(&mut device, 2, &[0.5, 0.25], 1);
        assert_eq!(device, [1.5, 1.5, 1.25, 1.25]);

        // Stereo, channels one after another, into interleaved.
        let mut device = [0.0; 4];
        mix_into(&mut device, 2, &[1.0, 2.0, 3.0, 4.0], 2);
        assert_eq!(device, [1.0, 3.0, 2.0, 4.0]);

        // Stereo on a mono device: averaged.
        let mut device = [0.0; 2];
        mix_into(&mut device, 1, &[1.0, 2.0, 3.0, 4.0], 2);
        assert_eq!(device, [2.0, 3.0]);

        // Stereo on four channels: the rest stay silent.
        let mut device = [0.0; 4];
        mix_into(&mut device, 4, &[1.0, 2.0], 2);
        assert_eq!(device, [1.0, 2.0, 0.0, 0.0]);

        // Four channels on stereo: the rest are left out.
        let mut device = [0.0; 2];
        mix_into(&mut device, 2, &[1.0, 2.0, 3.0, 4.0], 4);
        assert_eq!(device, [1.0, 2.0]);
    }

    #[test]
    fn the_sum_stays_in_device_range() {
        let mut run = Run::new();
        let source = run.add(SOURCE);
        for _ in 0..4 {
            let sink = run.add(SINK);
            run.connect(source, sink);
        }
        run.send_plan().unwrap();

        let device = run.render(MAX_FRAMES);

        assert!(device.iter().all(|sample| (-1.0..=1.0).contains(sample)));
        assert!(device.contains(&1.0), "four sines add up past the limit");
    }
}

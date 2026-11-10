//! A test harness: runs one node's processor without a host, the way a host would.
//!
//! Behind the `testing` feature, for the tests of plugins:
//!
//! ```toml
//! [dev-dependencies]
//! v-signal-nodes-plugin-api = { version = "…", features = ["testing"] }
//! ```
//!
//! A [`Harness`] starts with the spec's default parameters and changes them by key, so a
//! test names only the parameters it cares about, and a new parameter elsewhere in the
//! spec breaks nothing. It prepares the processor before the first block, counts the
//! position of the run from block to block like the host, and collects what the node
//! publishes to its plot windows.
//!
//! ```ignore
//! let mut node = Harness::new(&MyGain).sample_rate(1000.0);
//! node.set(GAIN, 0.5);
//! assert_eq!(node.run(&[&[1.0, -2.0]])[0], [0.5, -1.0]);
//! ```

use std::ffi::c_void;

use crate::{
    NodeProcessor, NodeProvider, NodeSpec, ParamKey, ParamValue, PlotTrace, PlotWindow,
    PluginError, ProcessIo, RawBuffer, RawPlotSink, SignalRate,
};

/// Samples per second unless [`Harness::sample_rate`] says otherwise.
pub const DEFAULT_SAMPLE_RATE: f32 = 48_000.0;
/// Largest block unless [`Harness::max_frames`] says otherwise.
pub const DEFAULT_MAX_FRAMES: usize = 1 << 16;

/// One node's processor with its parameters, run block by block.
pub struct Harness {
    spec: NodeSpec,
    processor: Box<dyn NodeProcessor>,
    params: Vec<ParamValue>,
    sample_rate: f32,
    max_frames: usize,
    /// Whether `prepare` was called (or is to be skipped).
    prepared: bool,
    position: u64,
    /// What each window of the spec got, in window order.
    plotted: Vec<Vec<Plotted>>,
}

/// A frame a node published to a plot window.
#[derive(Clone, Debug, PartialEq)]
pub struct Plotted {
    /// Distance between the samples on the horizontal axis.
    pub x_step: f32,
    /// The beams, each as long as the host got it (cut to `MAX_PLOT_POINTS`).
    pub beams: Vec<Vec<f32>>,
    /// How to draw it; `None` for a code no [`PlotTrace`] has.
    pub trace: Option<PlotTrace>,
}

impl Plotted {
    /// The only beam of a single-beam frame. Panics if there are several.
    pub fn samples(&self) -> &[f32] {
        assert_eq!(self.beams.len(), 1, "a frame of {} beams", self.beams.len());
        &self.beams[0]
    }
}

impl Harness {
    /// A new processor of `provider`, with the spec's default parameters.
    pub fn new(provider: &dyn NodeProvider) -> Self {
        Self::from_parts(provider.spec(), provider.create_processor())
    }

    /// `processor` of a node of type `spec` (one a host made, say), with the spec's
    /// default parameters.
    pub fn from_parts(spec: NodeSpec, processor: Box<dyn NodeProcessor>) -> Self {
        Self {
            params: spec.default_params(),
            plotted: spec.windows.iter().map(|_| Vec::new()).collect(),
            spec,
            processor,
            sample_rate: DEFAULT_SAMPLE_RATE,
            max_frames: DEFAULT_MAX_FRAMES,
            prepared: false,
            position: 0,
        }
    }

    /// Prepares the processor for `sample_rate` samples per second.
    pub fn sample_rate(mut self, sample_rate: f32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Prepares the processor for blocks of at most `max_frames`; longer ones panic.
    pub fn max_frames(mut self, max_frames: usize) -> Self {
        self.max_frames = max_frames;
        self
    }

    /// Doesn't prepare the processor before the first block, to see what it does
    /// unprepared; [`prepare`](Self::prepare) does it later.
    pub fn without_prepare(mut self) -> Self {
        self.prepared = true;
        self
    }

    /// The same harness with parameter `key` set to `value`, as [`set`](Self::set).
    pub fn with<K: ParamKey>(mut self, key: K, value: K::Value) -> Self
    where
        K::Value: Into<ParamValue>,
    {
        self.set(key, value);
        self
    }

    /// Sets parameter `key` to `value` for the next blocks. Panics if the spec has no
    /// such parameter or it doesn't accept the value (another kind, out of range): the
    /// host would never send it. [`set_raw`](Self::set_raw) sends it anyway.
    pub fn set<K: ParamKey>(&mut self, key: K, value: K::Value) -> &mut Self
    where
        K::Value: Into<ParamValue>,
    {
        let value = value.into();
        let index = self.param_index(key.id());
        assert!(
            self.spec.params[index].accepts(value),
            "parameter '{}' doesn't accept {value:?}",
            key.id()
        );
        self.params[index] = value;
        self
    }

    /// Sets parameter `id` to `value` unchecked, to see what the processor does with a
    /// value the host would never send. Panics if the spec has no such parameter.
    pub fn set_raw(&mut self, id: &str, value: ParamValue) -> &mut Self {
        let index = self.param_index(id);
        self.params[index] = value;
        self
    }

    /// The node's type.
    pub fn spec(&self) -> &NodeSpec {
        &self.spec
    }

    /// The parameters the next block gets, in spec order.
    pub fn params(&self) -> &[ParamValue] {
        &self.params
    }

    /// Frames of the run before the next block.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Starts the next block `position` frames into the run.
    pub fn set_position(&mut self, position: u64) -> &mut Self {
        self.position = position;
        self
    }

    /// Calls [`NodeProcessor::prepare`] with the harness's sample rate and block size.
    /// The first block does it on its own, unless [`without_prepare`](Self::without_prepare).
    pub fn prepare(&mut self) -> Result<(), PluginError> {
        self.prepared = true;
        self.processor.prepare(self.sample_rate, self.max_frames)
    }

    /// Calls [`NodeProcessor::reset`], and the run starts over at position 0, like
    /// in the host. What was plotted stays.
    pub fn reset(&mut self) {
        self.processor.reset();
        self.position = 0;
    }

    /// Processes one block of `frames` and returns the buffers of the outputs, in
    /// socket order, all channels of each. `inputs` holds one buffer per input socket,
    /// of its [`buffer_len`](crate::NodeSocket::buffer_len)`(frames)`. Prepares the
    /// processor first if it isn't yet, and moves the position on by `frames` if the
    /// block went through.
    ///
    /// Panics if the inputs don't fit the spec or `frames` is over the block size:
    /// the host would never call so.
    pub fn process(
        &mut self,
        frames: usize,
        inputs: &[&[f32]],
    ) -> Result<Vec<Vec<f32>>, PluginError> {
        assert!(
            frames <= self.max_frames,
            "{frames} frames, at most {}",
            self.max_frames
        );
        assert_eq!(
            inputs.len(),
            self.spec.inputs.len(),
            "one buffer per input socket"
        );
        for (index, (input, socket)) in inputs.iter().zip(&self.spec.inputs).enumerate() {
            assert_eq!(
                input.len(),
                socket.buffer_len(frames),
                "input {index} for {frames} frames"
            );
        }
        if !self.prepared {
            self.prepare()?;
        }

        let mut outputs: Vec<_> = self
            .spec
            .outputs
            .iter()
            .map(|socket| vec![0.0; socket.buffer_len(frames)])
            .collect();
        let raw_inputs: Vec<_> = inputs.iter().map(|input| RawBuffer::input(input)).collect();
        let raw_outputs: Vec<_> = outputs
            .iter_mut()
            .map(|output| RawBuffer::output(output))
            .collect();
        let sinks: Vec<_> = self
            .plotted
            .iter_mut()
            .map(|plotted| RawPlotSink {
                ctx: std::ptr::from_mut(plotted).cast(),
                publish: record,
            })
            .collect();
        // SAFETY: one buffer per socket of its length for `frames`, checked above; the
        // outputs are separate vectors that live for the call. One sink per window,
        // each with a vector of its own as `ctx` that nothing else touches meanwhile.
        let mut io = unsafe {
            ProcessIo::from_raw(frames, &self.spec, &self.params, &raw_inputs, &raw_outputs)
                .with_plots(&sinks)
        }
        .with_position(self.position);
        self.processor.process(&mut io)?;
        self.position += frames as u64;
        Ok(outputs)
    }

    /// [`process`](Self::process) of `inputs`, as many frames as they hold, which must
    /// work. Panics on an error, or if the node has no sample-rate input to count the
    /// frames by (use [`generate`](Self::generate) or `process` then).
    pub fn run(&mut self, inputs: &[&[f32]]) -> Vec<Vec<f32>> {
        let (index, socket) = self
            .spec
            .inputs
            .iter()
            .enumerate()
            .find(|(_, socket)| socket.rate == SignalRate::Sample)
            .expect("no sample-rate input to count the frames by");
        let frames =
            inputs.get(index).map_or(0, |input| input.len()) / usize::from(socket.channels);
        self.process(frames, inputs)
            .unwrap_or_else(|error| panic!("{}: {error}", self.spec.type_id))
    }

    /// [`process`](Self::process) of `frames` for a node without inputs, which must
    /// work. Panics on an error.
    pub fn generate(&mut self, frames: usize) -> Vec<Vec<f32>> {
        self.process(frames, &[])
            .unwrap_or_else(|error| panic!("{}: {error}", self.spec.type_id))
    }

    /// Everything published to plot window `key` so far, oldest first. Panics if the
    /// spec has no such window.
    pub fn plotted(&self, key: PlotWindow) -> &[Plotted] {
        &self.plotted[self.window_index(key)]
    }

    /// [`plotted`](Self::plotted), taken: the window starts empty again.
    pub fn take_plotted(&mut self, key: PlotWindow) -> Vec<Plotted> {
        let index = self.window_index(key);
        std::mem::take(&mut self.plotted[index])
    }

    fn param_index(&self, id: &str) -> usize {
        self.spec
            .param_index(id)
            .unwrap_or_else(|| panic!("{} has no parameter '{id}'", self.spec.type_id))
    }

    fn window_index(&self, key: PlotWindow) -> usize {
        self.spec
            .window_index(key.0)
            .unwrap_or_else(|| panic!("{} has no window '{}'", self.spec.type_id, key.0))
    }
}

/// The [`RawPlotSink::publish`] of the harness: copies the frame into the
/// `Vec<Plotted>` that `ctx` points at.
unsafe extern "C" fn record(
    ctx: *mut c_void,
    x_step: f32,
    samples: *const f32,
    len: usize,
    beams: usize,
    stride: usize,
    trace: u8,
) {
    // SAFETY: `Harness::process` passes a vector of its own as `ctx`, and the caller of
    // `publish` `beams` beams of `len` valid samples, `stride` apart.
    unsafe {
        let plotted = &mut *ctx.cast::<Vec<Plotted>>();
        let beams = (0..beams)
            .map(|beam| crate::slice_from_raw(samples.add(beam * stride), len).to_vec())
            .collect();
        plotted.push(Plotted {
            x_step,
            beams,
            trace: PlotTrace::from_code(trace),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeKind, NodeSocket, NumberParam, ToggleParam, Unit};

    const GAIN: NumberParam = NumberParam("gain");
    const ON: ToggleParam = ToggleParam("on");
    const SCREEN: PlotWindow = PlotWindow("screen");

    /// Multiplies by its gain, plots its input as two beams (it and its negation) and
    /// remembers whether `prepare` was called and at what rate.
    struct Probe;

    struct ProbeProcessor {
        sample_rate: Option<f32>,
    }

    impl NodeProvider for Probe {
        fn spec(&self) -> NodeSpec {
            NodeSpec::new("test.probe", "Probe", NodeKind::Processor)
                .input(NodeSocket::signal_in("In"))
                .output(NodeSocket::signal_out("Out"))
                .param(ON.spec("On", true))
                .param(GAIN.spec("Gain", -10.0, 10.0, 2.0))
                .window(SCREEN.spec("Screen", Unit::Seconds, Unit::Volts))
        }

        fn create_processor(&self) -> Box<dyn NodeProcessor> {
            Box::new(ProbeProcessor { sample_rate: None })
        }
    }

    impl NodeProcessor for ProbeProcessor {
        fn prepare(&mut self, sample_rate: f32, _max_frames: usize) -> Result<(), PluginError> {
            if sample_rate <= 0.0 {
                return Err(PluginError::new("bad rate"));
            }
            self.sample_rate = Some(sample_rate);
            Ok(())
        }

        fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
            let sample_rate = self
                .sample_rate
                .ok_or_else(|| PluginError::new("not prepared"))?;
            let gain = io.get(GAIN)?;
            let input = io.input(0);
            for (out, sample) in io.output(0).iter_mut().zip(input) {
                *out = sample * gain;
            }
            let mut beams = input.to_vec();
            beams.extend(input.iter().map(|sample| -sample));
            io.plot_beams(SCREEN, 1.0 / sample_rate, &beams, 2, PlotTrace::Lines)
        }
    }

    #[test]
    fn parameters_start_at_their_defaults_and_change_by_key() {
        let mut node = Harness::new(&Probe);
        assert_eq!(node.run(&[&[1.0, -1.0]]), [[2.0, -2.0]]);

        node.set(GAIN, 0.5);
        assert_eq!(node.run(&[&[1.0]]), [[0.5]]);
        assert_eq!(node.params()[0], ParamValue::Toggle(true), "the rest stays");

        node.set_raw("gain", ParamValue::Toggle(true));
        assert!(
            node.process(1, &[&[1.0]]).is_err(),
            "a wrong kind gets through"
        );
    }

    #[test]
    #[should_panic(expected = "doesn't accept")]
    fn values_the_host_would_refuse_panic() {
        Harness::new(&Probe).set(GAIN, 100.0);
    }

    #[test]
    #[should_panic(expected = "no parameter 'missing'")]
    fn unknown_parameters_panic() {
        Harness::new(&Probe).set(NumberParam("missing"), 1.0);
    }

    #[test]
    fn prepares_before_the_first_block_unless_told_not_to() {
        let mut node = Harness::new(&Probe).sample_rate(1000.0);
        node.run(&[&[1.0]]);
        assert_eq!(node.plotted(SCREEN)[0].x_step, 0.001);

        let mut node = Harness::new(&Probe).without_prepare();
        let error = node.process(1, &[&[1.0]]).unwrap_err();
        assert_eq!(error.message(), "not prepared");
        node.prepare().unwrap();
        assert!(node.process(1, &[&[1.0]]).is_ok());

        let mut node = Harness::new(&Probe).sample_rate(0.0);
        assert_eq!(
            node.process(1, &[&[1.0]]).unwrap_err().message(),
            "bad rate"
        );
    }

    #[test]
    fn the_position_moves_on_with_every_block_and_reset_starts_over() {
        let mut node = Harness::new(&Probe);
        node.run(&[&[0.0; 3]]);
        node.run(&[&[0.0; 4]]);
        assert_eq!(node.position(), 7);

        node.reset();
        assert_eq!(node.position(), 0);
        node.set_position(100).run(&[&[0.0]]);
        assert_eq!(node.position(), 101);
    }

    #[test]
    fn collects_the_plotted_frames_with_their_beams() {
        let mut node = Harness::new(&Probe).sample_rate(4.0);
        node.run(&[&[1.0, 2.0]]);
        node.run(&[&[3.0]]);

        let plotted = node.take_plotted(SCREEN);
        assert_eq!(
            plotted,
            [
                Plotted {
                    x_step: 0.25,
                    beams: vec![vec![1.0, 2.0], vec![-1.0, -2.0]],
                    trace: Some(PlotTrace::Lines),
                },
                Plotted {
                    x_step: 0.25,
                    beams: vec![vec![3.0], vec![-3.0]],
                    trace: Some(PlotTrace::Lines),
                },
            ]
        );
        assert!(node.plotted(SCREEN).is_empty(), "taken");
    }

    #[test]
    #[should_panic(expected = "input 0 for 2 frames")]
    fn inputs_that_dont_fit_the_spec_panic() {
        let _ = Harness::new(&Probe).process(2, &[&[1.0]]);
    }
}

//! [`OSCILLOGRAPH`]: shows a voltage over time in a window.
//!
//! An instrument at the end of a chain: it runs without an output node, and nothing
//! of what it looks at goes to the device. It works like a real one, sweep by sweep:
//!
//! 1. It waits for the trigger: the voltage crossing the level on the chosen slope,
//!    so a periodic signal stands still on the screen. If that doesn't happen within
//!    one sweep's time it starts anyway, so a constant voltage shows as well.
//! 2. It records one sweep, reducing long ones: each stretch of samples becomes its
//!    lowest and highest value, so short peaks stay visible however long the sweep.
//! 3. It publishes the sweep to its window and starts over.
//!
//! The editor draws the screen; a changed sweep time starts a new sweep.

use v_signal_nodes_plugin_api::{
    AxisUnit, ChoiceParam, MAX_PLOT_POINTS, NodeKind, NodeProcessor, NodeProvider, NodeSocket,
    NodeSpec, NumberParam, ParamKey, PlotWindow, PluginError, ProcessIo, TextKey,
};

use crate::OSCILLOGRAPH;

/// The window with the screen.
const SCREEN: PlotWindow = PlotWindow("screen");
/// How long one sweep takes, in milliseconds.
const SWEEP: NumberParam = NumberParam("sweep");
/// What starts a sweep; options in index order: free run, rising edge, falling edge.
const TRIGGER: ChoiceParam = ChoiceParam("trigger");
/// The voltage a trigger edge crosses.
const LEVEL: NumberParam = NumberParam("level");

/// Stretches of samples a long sweep is reduced to, two points each. A stretch has a
/// whole number of samples, so there can be up to half as many again.
const STRETCHES: usize = 1024;
/// Most points of a sweep: all of [`STRETCHES`] and half as many again, two points
/// each, with the last unfinished stretch.
const MAX_POINTS: usize = STRETCHES * 3 + 2;
const _: () = assert!(MAX_POINTS <= MAX_PLOT_POINTS);

pub(crate) struct Oscillograph;

impl NodeProvider for Oscillograph {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(OSCILLOGRAPH, "Oscillograph", NodeKind::Instrument)
            .input(NodeSocket::signal_in("In"))
            .param(SWEEP.spec("Sweep, ms", 0.01, 10_000.0, 10.0))
            .param(
                TRIGGER
                    .spec("Trigger", ["free run", "rising edge", "falling edge"], 1)
                    .advanced(),
            )
            .param(LEVEL.spec("Level, V", -1000.0, 1000.0, 0.0).advanced())
            .window(SCREEN.spec("Screen", AxisUnit::Seconds, AxisUnit::Volts))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Осциллограф"),
                    (TextKey::Input(0), "Вход"),
                    (SWEEP.name_key(), "Развёртка, мс"),
                    (TRIGGER.name_key(), "Запуск"),
                    (TRIGGER.option_key(0), "свободный"),
                    (TRIGGER.option_key(1), "по фронту"),
                    (TRIGGER.option_key(2), "по спаду"),
                    (LEVEL.name_key(), "Уровень, В"),
                    (SCREEN.name_key(), "Экран"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(OscillographProcessor {
            sample_rate: 0.0,
            sweep_frames: 0,
            sweeping: false,
            waited: 0,
            previous: 0.0,
            swept: 0,
            stretch: Stretch::EMPTY,
            points: Vec::new(),
        })
    }
}

/// The lowest and highest sample of a stretch being reduced.
#[derive(Clone, Copy)]
struct Stretch {
    frames: usize,
    low: f32,
    high: f32,
    /// The low came before the high, so the points stay in time order.
    low_first: bool,
}

impl Stretch {
    const EMPTY: Self = Self {
        frames: 0,
        low: f32::INFINITY,
        high: f32::NEG_INFINITY,
        low_first: true,
    };

    fn add(&mut self, sample: f32) {
        self.frames += 1;
        if sample < self.low {
            self.low = sample;
            self.low_first = false;
        }
        if sample > self.high {
            self.high = sample;
            self.low_first = true;
        }
    }

    /// Adds the stretch's points to `points`, if it has any.
    fn flush(&self, points: &mut Vec<f32>) {
        if self.frames == 0 {
            return;
        }
        let (first, second) = if self.low_first {
            (self.low, self.high)
        } else {
            (self.high, self.low)
        };
        points.push(first);
        points.push(second);
    }
}

struct OscillographProcessor {
    /// Samples per second, from `prepare`.
    sample_rate: f32,
    /// Frames of the sweep being recorded.
    sweep_frames: usize,
    sweeping: bool,
    /// Frames waited for the trigger so far.
    waited: usize,
    /// The sample before this one, to see an edge.
    previous: f32,
    /// Frames of the current sweep so far.
    swept: usize,
    stretch: Stretch,
    /// The current sweep; room for [`MAX_POINTS`] comes from `prepare`.
    points: Vec<f32>,
}

impl OscillographProcessor {
    /// Drops the sweep in progress and waits for the next trigger.
    fn start_over(&mut self) {
        self.sweeping = false;
        self.waited = 0;
        self.swept = 0;
        self.stretch = Stretch::EMPTY;
        self.points.clear();
    }

    /// Whether `sample` fires `trigger` (an index into its options) at `level`.
    fn fires(&self, sample: f32, trigger: usize, level: f32) -> bool {
        match trigger {
            1 => self.previous < level && sample >= level,
            2 => self.previous > level && sample <= level,
            _ => true,
        }
    }

    /// Records `sample`, reducing every `stride` samples to two points (none reduced
    /// if it's 1). Returns whether that completes the sweep.
    fn record(&mut self, sample: f32, stride: usize) -> bool {
        self.swept += 1;
        if stride == 1 {
            self.points.push(sample);
        } else {
            self.stretch.add(sample);
            if self.stretch.frames == stride {
                self.stretch.flush(&mut self.points);
                self.stretch = Stretch::EMPTY;
            }
        }
        if self.swept < self.sweep_frames {
            return false;
        }
        self.stretch.flush(&mut self.points);
        true
    }
}

impl NodeProcessor for OscillographProcessor {
    fn prepare(&mut self, sample_rate: f32, _max_frames: usize) -> Result<(), PluginError> {
        if !(sample_rate > 0.0 && sample_rate.is_finite()) {
            return Err(PluginError::new(format!(
                "invalid sample rate {sample_rate}"
            )));
        }
        self.sample_rate = sample_rate;
        self.points = Vec::with_capacity(MAX_POINTS);
        self.reset();
        Ok(())
    }

    fn reset(&mut self) {
        self.previous = 0.0;
        self.start_over();
    }

    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        if self.sample_rate == 0.0 {
            return Err(PluginError::new("not prepared"));
        }
        let trigger = io.get(TRIGGER)?;
        let level = io.get(LEVEL)?;
        let sweep_seconds = io.get(SWEEP)? * 0.001;
        let sweep_frames = ((sweep_seconds * self.sample_rate).round() as usize).max(2);
        if sweep_frames != self.sweep_frames {
            self.sweep_frames = sweep_frames;
            self.start_over();
        }
        let stride = (sweep_frames / STRETCHES).max(1);
        // Time of the whole sweep over its points, which also holds for reduced ones.
        let duration = sweep_frames as f32 / self.sample_rate;

        for &sample in io.input(0) {
            if !self.sweeping {
                self.waited += 1;
                self.sweeping =
                    self.fires(sample, trigger, level) || self.waited > self.sweep_frames;
            }
            self.previous = sample;
            if self.sweeping && self.record(sample, stride) {
                let x_step = duration / self.points.len() as f32;
                io.plot(SCREEN, x_step, &self.points)?;
                self.start_over();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use v_signal_nodes_plugin_api::{ParamValue, RawBuffer, RawPlotSink};

    /// Samples per second of the tests: one sweep of 1 ms has 1000 frames.
    const RATE: f32 = 1_000_000.0;

    /// A frame as `process` published it.
    type Frame = (f32, Vec<f32>);

    unsafe extern "C" fn record(ctx: *mut c_void, x_step: f32, samples: *const f32, len: usize) {
        // SAFETY: `Bench::run` passes its frames as `ctx`, and `plot` a valid slice.
        unsafe {
            let frames = &mut *ctx.cast::<Vec<Frame>>();
            frames.push((x_step, std::slice::from_raw_parts(samples, len).to_vec()));
        }
    }

    /// A prepared processor and what it published.
    struct Bench {
        spec: NodeSpec,
        processor: Box<dyn NodeProcessor>,
        frames: Vec<Frame>,
    }

    impl Bench {
        fn new() -> Self {
            let mut processor = Oscillograph.create_processor();
            processor.prepare(RATE, 1 << 16).unwrap();
            Self {
                spec: Oscillograph.spec(),
                processor,
                frames: Vec::new(),
            }
        }

        fn run(&mut self, sweep_ms: f32, trigger: usize, level: f32, input: &[f32]) {
            let params = [
                ParamValue::Number(sweep_ms),
                ParamValue::Choice(trigger),
                ParamValue::Number(level),
            ];
            let inputs = [RawBuffer::input(input)];
            let sinks = [RawPlotSink {
                ctx: std::ptr::from_mut(&mut self.frames).cast(),
                publish: record,
            }];
            // SAFETY: a mono input of `input.len()` frames, as the spec wants; the sink
            // writes to `frames`, which outlives the call and nothing else touches.
            let mut io = unsafe {
                ProcessIo::from_raw(input.len(), &self.spec, &params, &inputs, &[])
                    .with_plots(&sinks)
            };
            self.processor.process(&mut io).unwrap();
        }
    }

    /// A triangle wave within ±1, `period` frames long, starting at -1.
    fn triangle(period: usize, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|frame| {
                let phase = (frame % period) as f32 / period as f32;
                1.0 - 4.0 * (phase - 0.5).abs()
            })
            .collect()
    }

    #[test]
    fn a_sweep_starts_at_the_trigger_and_lasts_the_set_time() {
        let mut bench = Bench::new();

        // 1 ms is 1000 frames: fewer than twice the stretches, so nothing is reduced.
        bench.run(1.0, 1, 0.0, &triangle(400, 1500));

        let (x_step, points) = &bench.frames[0];
        assert_eq!(points.len(), 1000);
        assert!((x_step - 1.0 / RATE).abs() < 1e-9);
        // The triangle rises through 0 V a quarter of a period in.
        assert_eq!(points[0], 0.0);
        assert!(points[1] > 0.0);
    }

    #[test]
    fn a_falling_trigger_starts_on_the_way_down() {
        let mut bench = Bench::new();

        bench.run(1.0, 2, 0.0, &triangle(400, 1800));

        let points = &bench.frames[0].1;
        assert_eq!(points[0], 0.0);
        assert!(points[1] < 0.0);
    }

    #[test]
    fn without_a_trigger_it_starts_anyway() {
        let mut bench = Bench::new();

        bench.run(1.0, 1, 0.0, &[0.5; 2100]);

        assert_eq!(bench.frames.len(), 1, "after one sweep of waiting");
        assert!(bench.frames[0].1.iter().all(|&point| point == 0.5));
    }

    #[test]
    fn sweeps_continue_across_blocks() {
        let mut bench = Bench::new();
        let signal = triangle(400, 1500);

        for block in signal.chunks(64) {
            bench.run(1.0, 1, 0.0, block);
        }

        assert_eq!(bench.frames.len(), 1);
        assert_eq!(bench.frames[0].1.len(), 1000);
    }

    #[test]
    fn a_long_sweep_is_reduced_and_keeps_its_peaks() {
        let mut bench = Bench::new();
        // 10 ms, free run: 10 000 frames, one short spike in the middle.
        let mut signal = vec![0.0; 10_001];
        signal[5000] = 3.0;

        bench.run(10.0, 0, 0.0, &signal);

        let (x_step, points) = &bench.frames[0];
        assert!(points.len() <= MAX_POINTS, "{}", points.len());
        assert!(points.len() < 10_000);
        assert!(points.contains(&3.0), "the spike survives");
        let duration = x_step * points.len() as f32;
        assert!((duration - 0.01).abs() < 1e-5, "{duration}");
    }

    #[test]
    fn every_sweep_fits() {
        for sweep_frames in 2..STRETCHES * 40 {
            let stride = (sweep_frames / STRETCHES).max(1);
            let points = if stride == 1 {
                sweep_frames
            } else {
                sweep_frames.div_ceil(stride) * 2
            };
            assert!(
                points <= MAX_POINTS,
                "{sweep_frames} frames: {points} points"
            );
        }
    }

    #[test]
    fn a_changed_sweep_time_starts_over() {
        let mut bench = Bench::new();
        bench.run(1.0, 0, 0.0, &[1.0; 500]);

        bench.run(0.5, 0, 0.0, &[2.0; 500]);

        assert_eq!(bench.frames.len(), 1);
        assert_eq!(bench.frames[0].1, [2.0; 500], "nothing of the old sweep");
    }

    #[test]
    fn reset_drops_the_sweep_in_progress() {
        let mut bench = Bench::new();
        bench.run(1.0, 0, 0.0, &[1.0; 999]);

        bench.processor.reset();
        bench.run(1.0, 0, 0.0, &[2.0; 1000]);

        assert_eq!(bench.frames.len(), 1);
        assert!(bench.frames[0].1.iter().all(|&point| point == 2.0));
    }

    #[test]
    fn needs_a_valid_sample_rate() {
        let mut processor = Oscillograph.create_processor();
        assert!(processor.prepare(0.0, 16).is_err());
    }
}

//! [`DUAL_BEAM_OSCILLOSCOPE`]: two voltages over time on one screen.
//!
//! Works like the [`oscilloscope`](crate::oscilloscope), with one sweep for both
//! inputs: the trigger watches the input its advanced trigger source names, so the
//! other one is seen in time against it (a phase shift, a delay through a stage). Both
//! beams go to the screen as one frame, drawn over each other in their own colors.

use v_signal_nodes_plugin_api::{
    NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, TextKey,
};

use crate::DUAL_BEAM_OSCILLOSCOPE;
use crate::oscilloscope::{SweepProcessor, with_controls};

/// Beams, one per input.
const BEAMS: usize = 2;

pub(crate) struct DualBeamOscilloscope;

impl NodeProvider for DualBeamOscilloscope {
    fn spec(&self) -> NodeSpec {
        let spec = NodeSpec::new(
            DUAL_BEAM_OSCILLOSCOPE,
            "Dual-beam oscilloscope",
            NodeKind::Instrument,
        )
        .input(NodeSocket::signal_in("In 1"))
        .input(NodeSocket::signal_in("In 2"))
        .translate(
            "ru",
            [
                (TextKey::Name, "Двухлучевой осциллограф"),
                (TextKey::Input(0), "Вход 1"),
                (TextKey::Input(1), "Вход 2"),
            ],
        );
        with_controls(spec, BEAMS)
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(SweepProcessor::new(BEAMS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use v_signal_nodes_plugin_api::{
        MAX_PLOT_POINTS, ParamValue, PlotTrace, ProcessIo, RawBuffer, RawPlotSink,
    };

    /// Samples per second of the tests: one sweep of 1 ms has 1000 frames.
    const RATE: f32 = 1_000_000.0;

    /// A frame as `process` published it: x step, its beams, trace code.
    type Frame = (f32, Vec<Vec<f32>>, u8);

    unsafe extern "C" fn record(
        ctx: *mut c_void,
        x_step: f32,
        samples: *const f32,
        len: usize,
        beams: usize,
        stride: usize,
        trace: u8,
    ) {
        // SAFETY: `Bench::run` passes its frames as `ctx`, and `plot_beams` `beams`
        // valid slices of `len` samples, `stride` apart.
        unsafe {
            let frames = &mut *ctx.cast::<Vec<Frame>>();
            let beams = (0..beams)
                .map(|beam| std::slice::from_raw_parts(samples.add(beam * stride), len).to_vec())
                .collect();
            frames.push((x_step, beams, trace));
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
            let mut processor = DualBeamOscilloscope.create_processor();
            processor.prepare(RATE, 1 << 16).unwrap();
            Self {
                spec: DualBeamOscilloscope.spec(),
                processor,
                frames: Vec::new(),
            }
        }

        /// One block: `sweep_ms`, rising edge through 0 V on input `source`.
        fn run(&mut self, sweep_ms: f32, source: usize, interpolation: bool, inputs: [&[f32]; 2]) {
            let params = [
                ParamValue::Number(sweep_ms),
                ParamValue::Choice(1),
                ParamValue::Choice(source),
                ParamValue::Number(0.0),
                ParamValue::Toggle(interpolation),
            ];
            let buffers = inputs.map(RawBuffer::input);
            let sinks = [RawPlotSink {
                ctx: std::ptr::from_mut(&mut self.frames).cast(),
                publish: record,
            }];
            // SAFETY: two mono inputs of one length, as the spec wants; the sink writes
            // to `frames`, which outlives the call and nothing else touches.
            let mut io = unsafe {
                ProcessIo::from_raw(inputs[0].len(), &self.spec, &params, &buffers, &[])
                    .with_plots(&sinks)
            };
            self.processor.process(&mut io).unwrap();
        }
    }

    /// A triangle wave within ±1, `period` frames long, `shift` frames late, starting
    /// at -1.
    fn triangle(period: usize, shift: usize, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|frame| {
                let phase = ((frame + period - shift % period) % period) as f32 / period as f32;
                1.0 - 4.0 * (phase - 0.5).abs()
            })
            .collect()
    }

    #[test]
    fn the_params_are_in_the_order_the_bench_sets_them() {
        let ids: Vec<_> = DualBeamOscilloscope
            .spec()
            .params
            .iter()
            .map(|param| param.id.clone())
            .collect();
        assert_eq!(
            ids,
            ["sweep", "trigger", "source", "level", "interpolation"]
        );
    }

    #[test]
    fn one_sweep_records_both_beams_from_the_trigger_of_the_first() {
        let mut bench = Bench::new();
        let first = triangle(400, 0, 1500);
        // A quarter period late.
        let second = triangle(400, 100, 1500);

        bench.run(1.0, 0, false, [&first, &second]);

        let (x_step, beams, _) = &bench.frames[0];
        assert!((x_step - 1.0 / RATE).abs() < 1e-9);
        assert_eq!(beams.len(), 2);
        assert_eq!(beams[0].len(), 1000);
        assert_eq!(beams[1].len(), 1000);
        // The first rises through 0 V at the start; the second is a quarter period
        // behind, at its lowest.
        assert_eq!(beams[0][0], 0.0);
        assert!(beams[0][1] > 0.0);
        assert_eq!(beams[1][0], -1.0);
    }

    #[test]
    fn the_trigger_source_picks_the_input_that_starts_the_sweep() {
        let mut bench = Bench::new();
        let first = triangle(400, 0, 1700);
        let second = triangle(400, 100, 1700);

        bench.run(1.0, 1, false, [&first, &second]);

        let beams = &bench.frames[0].1;
        assert_eq!(
            beams[1][0], 0.0,
            "the second rises through 0 V at the start"
        );
        assert!(beams[1][1] > 0.0);
        assert_eq!(beams[0][0], 1.0, "a quarter period ahead, at its highest");
    }

    #[test]
    fn long_sweeps_reduce_both_beams_alike_and_keep_their_peaks() {
        let mut bench = Bench::new();
        // 10 ms: 10 000 frames from the rising edge at frame 1.
        let mut first = vec![0.0; 10_001];
        let mut second = vec![0.0; 10_001];
        first[0] = -1.0;
        first[3000] = 2.0;
        second[7000] = -3.0;

        bench.run(10.0, 0, true, [&first, &second]);

        let (x_step, beams, trace) = &bench.frames[0];
        assert_eq!(beams[0].len(), beams[1].len());
        assert!(beams[0].len() <= MAX_PLOT_POINTS);
        assert!(beams[0].contains(&2.0));
        assert!(beams[1].contains(&-3.0));
        assert_eq!(
            PlotTrace::from_code(*trace),
            Some(PlotTrace::Lines),
            "reduced"
        );
        let duration = x_step * beams[0].len() as f32;
        assert!((duration - 0.01).abs() < 1e-5, "{duration}");
    }

    #[test]
    fn short_sweeps_are_interpolated_when_asked() {
        let mut bench = Bench::new();
        let signal = triangle(400, 0, 1500);

        bench.run(1.0, 0, true, [&signal, &signal]);

        let trace = PlotTrace::from_code(bench.frames[0].2);
        assert_eq!(trace, Some(PlotTrace::Interpolated));
    }

    #[test]
    fn sweeps_continue_across_blocks() {
        let mut bench = Bench::new();
        let first = triangle(400, 0, 1500);
        let second = triangle(400, 100, 1500);

        for (a, b) in first.chunks(64).zip(second.chunks(64)) {
            bench.run(1.0, 0, false, [a, b]);
        }

        assert_eq!(bench.frames.len(), 1);
        assert_eq!(bench.frames[0].1[1].len(), 1000);
        assert_eq!(bench.frames[0].1[1][0], -1.0);
    }
}

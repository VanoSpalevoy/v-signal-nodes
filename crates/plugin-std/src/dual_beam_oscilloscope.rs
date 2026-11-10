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
    use crate::oscilloscope::{INTERPOLATION, SCREEN, SOURCE, SWEEP};
    use v_signal_nodes_plugin_api::testing::Harness;
    use v_signal_nodes_plugin_api::{MAX_PLOT_POINTS, PlotTrace};

    /// Samples per second of the tests: one sweep of 1 ms has 1000 frames.
    const RATE: f32 = 1_000_000.0;

    /// A dual-beam oscilloscope at [`RATE`], sweeping `sweep_ms` from a rising edge
    /// through 0 V (the default trigger) on input `source`.
    fn oscilloscope(sweep_ms: f32, source: usize) -> Harness {
        Harness::new(&DualBeamOscilloscope)
            .sample_rate(RATE)
            .with(SWEEP, sweep_ms)
            .with(SOURCE, source)
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
    fn one_sweep_records_both_beams_from_the_trigger_of_the_first() {
        let mut oscilloscope = oscilloscope(1.0, 0);
        let first = triangle(400, 0, 1500);
        // A quarter period late.
        let second = triangle(400, 100, 1500);

        oscilloscope.run(&[&first, &second]);

        let frame = &oscilloscope.plotted(SCREEN)[0];
        let beams = &frame.beams;
        assert!((frame.x_step - 1.0 / RATE).abs() < 1e-9);
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
        let mut oscilloscope = oscilloscope(1.0, 1);
        let first = triangle(400, 0, 1700);
        let second = triangle(400, 100, 1700);

        oscilloscope.run(&[&first, &second]);

        let beams = &oscilloscope.plotted(SCREEN)[0].beams;
        assert_eq!(
            beams[1][0], 0.0,
            "the second rises through 0 V at the start"
        );
        assert!(beams[1][1] > 0.0);
        assert_eq!(beams[0][0], 1.0, "a quarter period ahead, at its highest");
    }

    #[test]
    fn long_sweeps_reduce_both_beams_alike_and_keep_their_peaks() {
        let mut oscilloscope = oscilloscope(10.0, 0).with(INTERPOLATION, true);
        // 10 ms: 10 000 frames from the rising edge at frame 1.
        let mut first = vec![0.0; 10_001];
        let mut second = vec![0.0; 10_001];
        first[0] = -1.0;
        first[3000] = 2.0;
        second[7000] = -3.0;

        oscilloscope.run(&[&first, &second]);

        let frame = &oscilloscope.plotted(SCREEN)[0];
        let beams = &frame.beams;
        assert_eq!(beams[0].len(), beams[1].len());
        assert!(beams[0].len() <= MAX_PLOT_POINTS);
        assert!(beams[0].contains(&2.0));
        assert!(beams[1].contains(&-3.0));
        assert_eq!(frame.trace, Some(PlotTrace::Lines), "reduced");
        let duration = frame.x_step * beams[0].len() as f32;
        assert!((duration - 0.01).abs() < 1e-5, "{duration}");
    }

    #[test]
    fn short_sweeps_are_interpolated_when_asked() {
        let mut oscilloscope = oscilloscope(1.0, 0).with(INTERPOLATION, true);
        let signal = triangle(400, 0, 1500);

        oscilloscope.run(&[&signal, &signal]);

        let trace = oscilloscope.plotted(SCREEN)[0].trace;
        assert_eq!(trace, Some(PlotTrace::Interpolated));
    }

    #[test]
    fn sweeps_continue_across_blocks() {
        let mut oscilloscope = oscilloscope(1.0, 0);
        let first = triangle(400, 0, 1500);
        let second = triangle(400, 100, 1500);

        for (a, b) in first.chunks(64).zip(second.chunks(64)) {
            oscilloscope.run(&[a, b]);
        }

        let plotted = oscilloscope.plotted(SCREEN);
        assert_eq!(plotted.len(), 1);
        assert_eq!(plotted[0].beams[1].len(), 1000);
        assert_eq!(plotted[0].beams[1][0], -1.0);
    }
}

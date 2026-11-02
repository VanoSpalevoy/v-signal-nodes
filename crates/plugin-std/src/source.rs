//! [`SOURCE`]: a voltage generator.
//!
//! A periodic voltage of a set frequency, amplitude, starting phase and DC offset:
//! `offset + amplitude · waveform(frequency · time + phase)`, with the waveform within
//! ±1. Frequencies above half the sample rate can't be represented in the sampled
//! stream and stay at that limit.
//!
//! The phase comes from the time of the run ([`ProcessIo::position`]), not from a count
//! of its own. So generators of one frequency always keep the phases they are set to,
//! whenever they were added and whatever their frequency was before: 0° and 180° add up
//! to nothing. The price is a jump in the signal when the frequency changes.

use std::f64::consts::TAU;

use v_signal_nodes_plugin_api::{
    ChoiceParam, NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, NumberParam,
    ParamKey, PluginError, ProcessIo, TextKey,
};

use crate::SOURCE;

/// The waveform; options in index order: [`WAVEFORMS`].
const WAVEFORM: ChoiceParam = ChoiceParam("signal");
/// Options of [`WAVEFORM`].
const WAVEFORMS: [&str; 4] = ["sine", "square", "saw", "triangle"];
/// The frequency in Hz.
const FREQUENCY: NumberParam = NumberParam("frequency");
/// The amplitude in volts.
const AMPLITUDE: NumberParam = NumberParam("amplitude");
/// Where in its period the waveform is at time zero, in degrees.
const PHASE: NumberParam = NumberParam("phase");
/// The DC offset in volts.
const OFFSET: NumberParam = NumberParam("offset");

pub(crate) struct Source;

impl NodeProvider for Source {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(SOURCE, "Generator", NodeKind::Generator)
            .output(NodeSocket::signal_out("Out"))
            .param(WAVEFORM.spec("Waveform", WAVEFORMS, 0))
            .param(FREQUENCY.spec("Frequency, Hz", 0.1, 1_000_000.0, 1000.0))
            .param(AMPLITUDE.spec("Amplitude, V", 0.0, 1000.0, 1.0))
            .param(PHASE.spec("Phase, °", 0.0, 360.0, 0.0))
            .param(OFFSET.spec("Offset, V", -1000.0, 1000.0, 0.0).advanced())
            .translate(
                "ru",
                [
                    (TextKey::Name, "Генератор"),
                    (TextKey::Output(0), "Выход"),
                    (WAVEFORM.name_key(), "Форма"),
                    (WAVEFORM.option_key(0), "синус"),
                    (WAVEFORM.option_key(1), "меандр"),
                    (WAVEFORM.option_key(2), "пила"),
                    (WAVEFORM.option_key(3), "треугольник"),
                    (FREQUENCY.name_key(), "Частота, Гц"),
                    (AMPLITUDE.name_key(), "Амплитуда, В"),
                    (PHASE.name_key(), "Фаза, °"),
                    (OFFSET.name_key(), "Смещение, В"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(SourceProcessor { sample_rate: 0.0 })
    }
}

struct SourceProcessor {
    /// Samples per second, from `prepare`.
    sample_rate: f64,
}

impl NodeProcessor for SourceProcessor {
    fn prepare(&mut self, sample_rate: f32, _max_frames: usize) -> Result<(), PluginError> {
        if !(sample_rate > 0.0 && sample_rate.is_finite()) {
            return Err(PluginError::new(format!(
                "invalid sample rate {sample_rate}"
            )));
        }
        self.sample_rate = f64::from(sample_rate);
        Ok(())
    }

    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        if self.sample_rate == 0.0 {
            return Err(PluginError::new("not prepared"));
        }
        let waveform = io.get(WAVEFORM)?;
        let cycles_per_frame = (f64::from(io.get(FREQUENCY)?) / self.sample_rate).min(0.5);
        let amplitude = io.get(AMPLITUDE)?;
        let shift = f64::from(io.get(PHASE)?) / 360.0;
        let offset = io.get(OFFSET)?;
        let start = io.position();
        for (frame, sample) in io.output(0).iter_mut().enumerate() {
            let time = (start + frame as u64) as f64;
            // The whole periods go first, so `shift` adds to a small number exactly and
            // two generators differ by it precisely, however long the run has lasted.
            let phase = ((cycles_per_frame * time).fract() + shift).fract();
            let value = match waveform {
                0 => (phase * TAU).sin(),
                1 => {
                    if phase < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                2 => 2.0 * phase - 1.0,
                _ => 1.0 - 4.0 * (phase - 0.5).abs(),
            };
            *sample = offset + amplitude * value as f32;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{prepared, run, run_at};
    use v_signal_nodes_plugin_api::ParamValue;

    fn params(waveform: usize, frequency: f32, amplitude: f32, offset: f32) -> [ParamValue; 5] {
        with_phase(waveform, frequency, amplitude, 0.0, offset)
    }

    fn with_phase(
        waveform: usize,
        frequency: f32,
        amplitude: f32,
        phase: f32,
        offset: f32,
    ) -> [ParamValue; 5] {
        [
            ParamValue::Choice(waveform),
            ParamValue::Number(frequency),
            ParamValue::Number(amplitude),
            ParamValue::Number(phase),
            ParamValue::Number(offset),
        ]
    }

    #[test]
    fn the_waveform_follows_the_time_of_the_run() {
        // A 1 Hz square at 4 samples per second: two highs, two lows, repeating.
        let params = params(1, 1.0, 1.0, 0.0);
        let mut processor = prepared(&Source);
        let mut block =
            |position| run_at(position, &Source, processor.as_mut(), &params, &[], 3).unwrap();
        assert_eq!(block(0), [1.0, 1.0, -1.0]);
        assert_eq!(block(3), [-1.0, 1.0, 1.0]);
        // No history: a new processor at the same time gives the same.
        let mut late = prepared(&Source);
        let output = run_at(3, &Source, late.as_mut(), &params, &[], 3).unwrap();
        assert_eq!(output, [-1.0, 1.0, 1.0]);
    }

    #[test]
    fn the_phase_shifts_the_start_of_the_waveform() {
        let mut processor = prepared(&Source);
        let at = |phase| with_phase(0, 1.0, 2.0, phase, 0.0);

        // A sine at a quarter of its period starts at its peak.
        let output = run(&Source, processor.as_mut(), &at(90.0), &[], 1).unwrap();
        assert_eq!(output, [2.0]);
        // A full turn is the same as none.
        let full = run(&Source, processor.as_mut(), &at(360.0), &[], 4).unwrap();
        let none = run(&Source, processor.as_mut(), &at(0.0), &[], 4).unwrap();
        assert_eq!(full, none);
    }

    #[test]
    fn waveforms_in_antiphase_cancel_out_also_far_into_a_run() {
        let mut processor = prepared(&Source);
        // A day at 4 samples per second, and a frequency without a neat period.
        let position = 345_600;
        for waveform in [0, 1, 3] {
            let at = |phase| with_phase(waveform, 0.37, 7.0, phase, 0.0);
            let mut signal =
                |phase| run_at(position, &Source, processor.as_mut(), &at(phase), &[], 16);
            let first = signal(0.0).unwrap();
            let second = signal(180.0).unwrap();
            for (a, b) in first.iter().zip(&second) {
                assert!((a + b).abs() < 1e-5, "waveform {waveform}: {a} + {b}");
            }
        }
    }

    #[test]
    fn the_voltage_has_the_set_amplitude_and_offset() {
        let mut processor = prepared(&Source);
        let params = params(1, 1.0, 5.0, 2.0);

        let output = run(&Source, processor.as_mut(), &params, &[], 4).unwrap();

        assert_eq!(output, [7.0, 7.0, -3.0, -3.0]);
    }

    #[test]
    fn the_frequency_stops_at_half_the_sample_rate() {
        let mut processor = prepared(&Source);
        let params = params(1, 1000.0, 1.0, 0.0);

        let output = run(&Source, processor.as_mut(), &params, &[], 4).unwrap();

        assert_eq!(
            output,
            [1.0, -1.0, 1.0, -1.0],
            "2 Hz at 4 samples per second"
        );
    }

    #[test]
    fn needs_a_valid_sample_rate() {
        let mut processor = Source.create_processor();
        assert!(processor.prepare(0.0, 16).is_err());
        let params = params(0, 1.0, 1.0, 0.0);
        assert!(run(&Source, processor.as_mut(), &params, &[], 1).is_err());
    }
}

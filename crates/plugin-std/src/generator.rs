//! [`GENERATOR`]: a voltage generator.
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
    ParamKey, PluginError, ProcessIo, TextKey, Unit,
};

use crate::GENERATOR;

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

pub(crate) struct Generator;

impl NodeProvider for Generator {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(GENERATOR, "Generator", NodeKind::Generator)
            .output(NodeSocket::signal_out("Out"))
            .param(WAVEFORM.spec("Waveform", WAVEFORMS, 0))
            .param(
                FREQUENCY
                    .spec("Frequency, Hz", 0.1, 1_000_000.0, 1000.0)
                    .unit(Unit::Hertz),
            )
            .param(
                AMPLITUDE
                    .spec("Amplitude, V", 0.0, 1000.0, 1.0)
                    .unit(Unit::Volts),
            )
            .param(PHASE.spec("Phase, °", 0.0, 360.0, 0.0).unit(Unit::Degrees))
            .param(
                OFFSET
                    .spec("Offset, V", -1000.0, 1000.0, 0.0)
                    .unit(Unit::Volts)
                    .advanced(),
            )
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
        Box::new(GeneratorProcessor { sample_rate: 0.0 })
    }
}

struct GeneratorProcessor {
    /// Samples per second, from `prepare`.
    sample_rate: f64,
}

impl NodeProcessor for GeneratorProcessor {
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
    use v_signal_nodes_plugin_api::testing::Harness;

    /// Square waveform, the option index of "square".
    const SQUARE: usize = 1;

    /// A generator of 1 Hz at 4 samples per second: a period is 4 samples.
    fn generator(waveform: usize) -> Harness {
        Harness::new(&Generator)
            .sample_rate(4.0)
            .with(WAVEFORM, waveform)
            .with(FREQUENCY, 1.0)
    }

    #[test]
    fn the_waveform_follows_the_time_of_the_run() {
        // Two highs, two lows, repeating.
        let mut square = generator(SQUARE);
        assert_eq!(square.generate(3), [[1.0, 1.0, -1.0]]);
        assert_eq!(square.generate(3), [[-1.0, 1.0, 1.0]]);
        // No history: a new processor at the same time gives the same.
        let mut late = generator(SQUARE);
        late.set_position(3);
        assert_eq!(late.generate(3), [[-1.0, 1.0, 1.0]]);
    }

    #[test]
    fn the_phase_shifts_the_start_of_the_waveform() {
        let mut sine = generator(0).with(AMPLITUDE, 2.0);
        let mut from_start = |phase| {
            sine.set(PHASE, phase).set_position(0);
            sine.generate(4).remove(0)
        };

        // A sine at a quarter of its period starts at its peak.
        assert_eq!(from_start(90.0)[0], 2.0);
        // A full turn is the same as none.
        assert_eq!(from_start(360.0), from_start(0.0));
    }

    #[test]
    fn waveforms_in_antiphase_cancel_out_also_far_into_a_run() {
        // A day at 4 samples per second, and a frequency without a neat period.
        let position = 345_600;
        for waveform in [0, 1, 3] {
            let mut generator = generator(waveform)
                .with(FREQUENCY, 0.37)
                .with(AMPLITUDE, 7.0);
            let mut signal = |phase| {
                generator.set(PHASE, phase).set_position(position);
                generator.generate(16).remove(0)
            };
            let first = signal(0.0);
            let second = signal(180.0);
            for (a, b) in first.iter().zip(&second) {
                assert!((a + b).abs() < 1e-5, "waveform {waveform}: {a} + {b}");
            }
        }
    }

    #[test]
    fn the_voltage_has_the_set_amplitude_and_offset() {
        let mut square = generator(SQUARE).with(AMPLITUDE, 5.0).with(OFFSET, 2.0);

        assert_eq!(square.generate(4), [[7.0, 7.0, -3.0, -3.0]]);
    }

    #[test]
    fn the_frequency_stops_at_half_the_sample_rate() {
        let mut square = generator(SQUARE).with(FREQUENCY, 1000.0);

        assert_eq!(
            square.generate(4),
            [[1.0, -1.0, 1.0, -1.0]],
            "2 Hz at 4 samples per second"
        );
    }

    #[test]
    fn needs_a_valid_sample_rate() {
        assert!(generator(0).sample_rate(0.0).process(1, &[]).is_err());
        let mut unprepared = generator(0).without_prepare();
        assert!(unprepared.process(1, &[]).is_err());
    }
}

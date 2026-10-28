//! The standard plugin: built-in node types (`std.*`).
//!
//! | Type id      | Kind      | Inputs            | Outputs          | Parameters        |
//! |--------------|-----------|-------------------|------------------|-------------------|
//! | `std.source` | Generator | —                 | Audio            | Signal, Frequency |
//! | `std.gain`   | Processor | Audio in          | Audio out        | Gain              |
//! | `std.mixer`  | Processor | Left, Right, Side | Mixed (stereo)   | Mode, Gain, Pan   |
//! | `std.sink`   | Output    | Audio (stereo)    | —                | —                 |
//!
//! All sockets are audio rate and mono unless marked stereo. Every label is
//! English in the spec, with Russian translations next to it
//! (`NodeSpec::translate`), so the editor needs no strings of its own for these
//! nodes. Parameters are declared once as key constants, used both for the spec
//! and for reading the values in `process`.
//!
//! This is the reference plugin: other plugins will copy from it, so it keeps to
//! the rules a processor has to follow (sample rate from `prepare`, no
//! allocations in `process`).

use std::f32::consts::TAU;

use v_signal_nodes_plugin_api::{
    ChoiceParam, NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, NumberParam,
    ParamKey, Plugin, PluginError, ProcessIo, Registrar, TextKey,
};

/// Type id of the signal generator.
pub const SOURCE: &str = "std.source";
/// Type id of the gain (volume) node.
pub const GAIN: &str = "std.gain";
/// Type id of the mixer.
pub const MIXER: &str = "std.mixer";
/// Type id of the signal sink (graph output).
pub const SINK: &str = "std.sink";

/// The standard plugin, named `"std"`.
pub struct StdPlugin;

impl Plugin for StdPlugin {
    fn name(&self) -> &str {
        "std"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(&self, registrar: &mut dyn Registrar) {
        registrar.register_node(Box::new(Source));
        registrar.register_node(Box::new(Gain));
        registrar.register_node(Box::new(Mixer));
        registrar.register_node(Box::new(Sink));
    }
}

/// [`SOURCE`]: a periodic waveform at a set frequency.
struct Source;

/// The source's waveform; options in index order: [`WAVEFORMS`].
const SIGNAL: ChoiceParam = ChoiceParam("signal");
/// Options of [`SIGNAL`].
const WAVEFORMS: [&str; 4] = ["sine", "square", "saw", "triangle"];
/// The source's frequency in Hz.
const FREQUENCY: NumberParam = NumberParam("frequency");

impl NodeProvider for Source {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(SOURCE, "Source", NodeKind::Generator)
            .output(NodeSocket::signal_out("Audio"))
            .param(SIGNAL.spec("Signal", WAVEFORMS, 0))
            .param(FREQUENCY.spec("Frequency", 20.0, 20_000.0, 440.0))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Источник"),
                    (TextKey::Output(0), "Аудио"),
                    (SIGNAL.name_key(), "Сигнал"),
                    (SIGNAL.option_key(0), "синус"),
                    (SIGNAL.option_key(1), "меандр"),
                    (SIGNAL.option_key(2), "пила"),
                    (SIGNAL.option_key(3), "треугольник"),
                    (FREQUENCY.name_key(), "Частота"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(SourceProcessor {
            sample_rate: 0.0,
            phase: 0.0,
        })
    }
}

struct SourceProcessor {
    /// Samples per second, from `prepare`.
    sample_rate: f32,
    /// Position in the current period, `0.0..1.0`.
    phase: f32,
}

impl NodeProcessor for SourceProcessor {
    fn prepare(&mut self, sample_rate: f32, _max_frames: usize) -> Result<(), PluginError> {
        if !(sample_rate > 0.0 && sample_rate.is_finite()) {
            return Err(PluginError::new(format!(
                "invalid sample rate {sample_rate}"
            )));
        }
        self.sample_rate = sample_rate;
        Ok(())
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }

    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        if self.sample_rate == 0.0 {
            return Err(PluginError::new("not prepared"));
        }
        let waveform = io.get(SIGNAL)?;
        let step = io.get(FREQUENCY)? / self.sample_rate;
        for sample in io.output(0) {
            let phase = self.phase;
            *sample = match waveform {
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
            self.phase = (phase + step).fract();
        }
        Ok(())
    }
}

/// [`GAIN`]: multiplies the signal by a factor.
struct Gain;

/// The gain factor, shared by [`Gain`] and [`Mixer`].
const GAIN_FACTOR: NumberParam = NumberParam("gain");

impl NodeProvider for Gain {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(GAIN, "Gain", NodeKind::Processor)
            .input(NodeSocket::signal_in("Audio in"))
            .output(NodeSocket::signal_out("Audio out"))
            .param(GAIN_FACTOR.spec("Gain", 0.0, 2.0, 1.0))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Усилитель"),
                    (TextKey::Input(0), "Аудио вход"),
                    (TextKey::Output(0), "Аудио выход"),
                    (GAIN_FACTOR.name_key(), "Усиление"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(GainProcessor)
    }
}

struct GainProcessor;

impl NodeProcessor for GainProcessor {
    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        let gain = io.get(GAIN_FACTOR)?;
        let input = io.input(0);
        for (out, sample) in io.output(0).iter_mut().zip(input) {
            *out = sample * gain;
        }
        Ok(())
    }
}

/// [`MIXER`]: mixes three mono inputs into a stereo output, scaled by `gain`.
///
/// In stereo mode Left goes to the left channel, Right to the right one, and
/// Side is panned between them by `pan` (linearly: left, half each, right). In
/// mono mode all three are summed into both channels.
struct Mixer;

/// Mono or stereo mixing; options: `mono`, `stereo`.
const MODE: ChoiceParam = ChoiceParam("mode");
/// Where the Side input goes in stereo mode; options: `left`, `center`, `right`.
const PAN: ChoiceParam = ChoiceParam("pan");

impl NodeProvider for Mixer {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(MIXER, "Mixer", NodeKind::Processor)
            .input(NodeSocket::signal_in("Left"))
            .input(NodeSocket::signal_in("Right"))
            .input(NodeSocket::signal_in("Side"))
            .output(NodeSocket::signal_out("Mixed").with_channels(2))
            .param(MODE.spec("Mode", ["mono", "stereo"], 1))
            .param(GAIN_FACTOR.spec("Gain", 0.0, 2.0, 1.0))
            .param(PAN.spec("Pan", ["left", "center", "right"], 1))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Микшер"),
                    (TextKey::Input(0), "Левый"),
                    (TextKey::Input(1), "Правый"),
                    (TextKey::Input(2), "Боковой"),
                    (TextKey::Output(0), "Микс"),
                    (MODE.name_key(), "Режим"),
                    (MODE.option_key(0), "моно"),
                    (MODE.option_key(1), "стерео"),
                    (GAIN_FACTOR.name_key(), "Усиление"),
                    (PAN.name_key(), "Панорама"),
                    (PAN.option_key(0), "лево"),
                    (PAN.option_key(1), "центр"),
                    (PAN.option_key(2), "право"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(MixerProcessor)
    }
}

struct MixerProcessor;

impl NodeProcessor for MixerProcessor {
    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        let gain = io.get(GAIN_FACTOR)?;
        let stereo = io.get(MODE)? == 1;
        // Share of Side in the right channel.
        let pan = match io.get(PAN)? {
            0 => 0.0,
            1 => 0.5,
            _ => 1.0,
        };
        let (left, right, side) = (io.input(0), io.input(1), io.input(2));
        let mut channels = io.output_channels(0);
        let (Some(out_left), Some(out_right)) = (channels.next(), channels.next()) else {
            // An empty block: nothing to write.
            return Ok(());
        };
        let frames = out_left.iter_mut().zip(out_right.iter_mut());
        for (frame, (out_left, out_right)) in frames.enumerate() {
            let (l, r, s) = (left[frame], right[frame], side[frame]);
            let (mixed_left, mixed_right) = if stereo {
                (l + s * (1.0 - pan), r + s * pan)
            } else {
                let sum = l + r + s;
                (sum, sum)
            };
            *out_left = mixed_left * gain;
            *out_right = mixed_right * gain;
        }
        Ok(())
    }
}

/// [`SINK`]: the end of the graph; the host reads its input.
struct Sink;

impl NodeProvider for Sink {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(SINK, "Sink", NodeKind::Output)
            .input(NodeSocket::signal_in("Audio").with_channels(2))
            .translate(
                "ru",
                [(TextKey::Name, "Приёмник"), (TextKey::Input(0), "Аудио")],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(SinkProcessor)
    }
}

/// Does nothing: the sink has no outputs to fill.
struct SinkProcessor;

impl NodeProcessor for SinkProcessor {
    fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_plugin_api::{ParamValue, RawBuffer};

    fn providers() -> Vec<Box<dyn NodeProvider>> {
        vec![
            Box::new(Source),
            Box::new(Gain),
            Box::new(Mixer),
            Box::new(Sink),
        ]
    }

    /// A new processor of `provider`, prepared for 4 samples per second.
    fn prepared(provider: &dyn NodeProvider) -> Box<dyn NodeProcessor> {
        let mut processor = provider.create_processor();
        processor.prepare(4.0, 16).unwrap();
        processor
    }

    /// Runs a processor of `provider` once over `inputs` (one buffer per input
    /// socket) and returns the buffer of its first output, all channels (empty
    /// for a node without outputs).
    fn run(
        provider: &dyn NodeProvider,
        processor: &mut dyn NodeProcessor,
        params: &[ParamValue],
        inputs: &[&[f32]],
        frames: usize,
    ) -> Result<Vec<f32>, PluginError> {
        let spec = provider.spec();
        let mut output_samples: Vec<_> = spec
            .outputs
            .iter()
            .map(|socket| vec![0.0; socket.buffer_len(frames)])
            .collect();
        let inputs: Vec<_> = inputs
            .iter()
            .map(|samples| RawBuffer::input(samples))
            .collect();
        let raw_outputs: Vec<_> = output_samples
            .iter_mut()
            .map(|samples| RawBuffer::output(samples))
            .collect();
        // SAFETY: the tests pass one buffer per input socket, of its length; the
        // outputs are sized from the spec, live for the call and are separate
        // vectors.
        let mut io = unsafe { ProcessIo::from_raw(frames, &spec, params, &inputs, &raw_outputs) };
        processor.process(&mut io)?;
        Ok(output_samples.into_iter().next().unwrap_or_default())
    }

    #[test]
    fn specs_are_valid_and_fully_translated() {
        for provider in providers() {
            let spec = provider.spec();
            assert!(spec.validate().is_ok(), "{}", spec.type_id);
            for key in spec.text_keys() {
                assert_ne!(
                    spec.label("ru", key),
                    spec.label("en", key),
                    "{} {key:?}",
                    spec.type_id
                );
            }
        }
    }

    #[test]
    fn gain_scales_the_input() {
        let mut processor = prepared(&Gain);
        let output = run(
            &Gain,
            processor.as_mut(),
            &[ParamValue::Number(0.5)],
            &[&[1.0, -2.0]],
            2,
        );
        assert_eq!(output.unwrap(), [0.5, -1.0]);
    }

    #[test]
    fn mixer_mixes_into_stereo() {
        let mut processor = prepared(&Mixer);
        let inputs: [&[f32]; 3] = [&[1.0, 0.0], &[0.5, 1.0], &[0.25, -1.0]];
        let mut mix = |mode, gain, pan| {
            let params = [
                ParamValue::Choice(mode),
                ParamValue::Number(gain),
                ParamValue::Choice(pan),
            ];
            run(&Mixer, processor.as_mut(), &params, &inputs, 2).unwrap()
        };

        // Stereo, Side in the center: left channel, then right channel.
        assert_eq!(mix(1, 1.0, 1), [1.125, -0.5, 0.625, 0.5]);
        // Side fully right, doubled.
        assert_eq!(mix(1, 2.0, 2), [2.0, 0.0, 1.5, 0.0]);
        // Mono: everything in both channels.
        assert_eq!(mix(0, 1.0, 0), [1.75, 0.0, 1.75, 0.0]);
    }

    #[test]
    fn source_generates_the_waveform_across_calls() {
        // A 1 Hz square at 4 samples per second: two highs, two lows, repeating.
        let params = [ParamValue::Choice(1), ParamValue::Number(1.0)];
        let mut processor = prepared(&Source);
        let mut block = || run(&Source, processor.as_mut(), &params, &[], 3).unwrap();
        assert_eq!(block(), [1.0, 1.0, -1.0]);
        assert_eq!(block(), [-1.0, 1.0, 1.0]);

        // `reset` starts the period over.
        processor.reset();
        assert_eq!(
            run(&Source, processor.as_mut(), &params, &[], 3).unwrap(),
            [1.0, 1.0, -1.0]
        );
    }

    #[test]
    fn source_needs_a_valid_sample_rate() {
        let mut processor = Source.create_processor();
        assert!(processor.prepare(0.0, 16).is_err());
        let params = [ParamValue::Choice(0), ParamValue::Number(1.0)];
        assert!(run(&Source, processor.as_mut(), &params, &[], 1).is_err());
    }

    #[test]
    fn wrong_parameter_kinds_are_errors() {
        let mut processor = prepared(&Gain);
        let wrong = [ParamValue::Choice(0)];
        let error = run(&Gain, processor.as_mut(), &wrong, &[&[1.0]], 1).unwrap_err();
        assert!(error.message().contains("'gain'"), "{error}");
    }
}

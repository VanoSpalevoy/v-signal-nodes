//! The standard plugin: built-in node types (`std.*`) for analog electrical signals.
//!
//! A signal here is a voltage: every sample is the voltage at that moment, in volts,
//! and the stream is sampled finely enough to stand for a continuous signal. All
//! sockets are mono, one conductor each.
//!
//! | Type id                 | Module                | Kind       | Inputs     | Outputs | Parameters                                 |
//! |-------------------------|-----------------------|------------|------------|---------|--------------------------------------------|
//! | `std.generator`         | [`generator`]         | Generator  | —          | Out     | Waveform, Frequency, Amplitude, Phase, *Offset* |
//! | `std.amplifier`         | [`amplifier`]         | Processor  | In         | Out     | Gain, *Clipping, Supply*                   |
//! | `std.summing_amplifier` | [`summing_amplifier`] | Processor  | In 1, 2, 3 | Out     | Gain, *Weight 1, 2, 3*, *Clipping, Supply* |
//! | `std.device_output`     | [`device_output`]     | Output     | In         | —       | —                                          |
//! | `std.oscillograph`      | [`oscillograph`]      | Instrument | In         | —       | Sweep, *Trigger*, *Level*                  |
//!
//! Parameters in italics are advanced (`ParamSpec::advanced`). The oscillograph also
//! has a window with its screen. Every label is English in the spec, with Russian
//! translations next to it (`NodeSpec::translate`), so the editor needs no strings of
//! its own for these nodes. Parameters are declared once as key constants, used both
//! for the spec and for reading the values in `process`.
//!
//! This is the reference plugin: other plugins will copy from it, so it keeps to
//! the rules a processor has to follow (sample rate from `prepare`, no
//! allocations in `process`).

pub mod amplifier;
pub mod device_output;
pub mod generator;
pub mod oscillograph;
pub mod summing_amplifier;

use v_signal_nodes_plugin_api::{
    NumberParam, ParamKey, ParamSpec, Plugin, PluginError, ProcessIo, Registrar, TextKey,
    ToggleParam,
};

/// Type id of the voltage generator.
pub const GENERATOR: &str = "std.generator";
/// Type id of the amplifier.
pub const AMPLIFIER: &str = "std.amplifier";
/// Type id of the summing amplifier.
pub const SUMMING_AMPLIFIER: &str = "std.summing_amplifier";
/// Type id of the oscillograph.
pub const OSCILLOGRAPH: &str = "std.oscillograph";
/// Type id of the output to the device.
pub const DEVICE_OUTPUT: &str = "std.device_output";

/// Whether an amplifier stage clips at its [`SUPPLY`], like a real one. Off by default:
/// an ideal stage does what its gain says, however high the voltage.
const CLIPPING: ToggleParam = ToggleParam("clipping");
/// The supply voltage of an amplifier stage: with [`CLIPPING`] on, its output stays
/// within ± this.
const SUPPLY: NumberParam = NumberParam("supply");

/// The specs of [`CLIPPING`] and [`SUPPLY`], both advanced.
fn clipping_specs() -> [ParamSpec; 2] {
    [
        CLIPPING.spec("Clipping", false).advanced(),
        SUPPLY.spec("Supply, V", 0.1, 1000.0, 15.0).advanced(),
    ]
}

/// The translations of [`CLIPPING`] and [`SUPPLY`].
fn clipping_translations() -> [(TextKey, &'static str); 2] {
    [
        (CLIPPING.name_key(), "Ограничение"),
        (SUPPLY.name_key(), "Питание, В"),
    ]
}

/// The highest voltage an amplifier stage puts out in this block, either way.
fn output_limit(io: &ProcessIo<'_>) -> Result<f32, PluginError> {
    Ok(if io.get(CLIPPING)? {
        io.get(SUPPLY)?
    } else {
        f32::INFINITY
    })
}

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
        registrar.register_node(Box::new(generator::Generator));
        registrar.register_node(Box::new(amplifier::Amplifier));
        registrar.register_node(Box::new(summing_amplifier::SummingAmplifier));
        registrar.register_node(Box::new(device_output::DeviceOutput));
        registrar.register_node(Box::new(oscillograph::Oscillograph));
    }
}

#[cfg(test)]
mod testing {
    //! Running processors of this plugin without a host.

    use v_signal_nodes_plugin_api::{
        NodeProcessor, NodeProvider, ParamValue, PluginError, ProcessIo, RawBuffer,
    };

    /// A new processor of `provider`, prepared for 4 samples per second.
    pub(crate) fn prepared(provider: &dyn NodeProvider) -> Box<dyn NodeProcessor> {
        let mut processor = provider.create_processor();
        processor.prepare(4.0, 16).unwrap();
        processor
    }

    /// Runs a processor of `provider` once over `inputs` (one buffer per input
    /// socket) and returns the buffer of its first output, all channels (empty
    /// for a node without outputs).
    pub(crate) fn run(
        provider: &dyn NodeProvider,
        processor: &mut dyn NodeProcessor,
        params: &[ParamValue],
        inputs: &[&[f32]],
        frames: usize,
    ) -> Result<Vec<f32>, PluginError> {
        run_at(0, provider, processor, params, inputs, frames)
    }

    /// [`run`] for a block starting `position` frames into the run.
    pub(crate) fn run_at(
        position: u64,
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
        let mut io = unsafe { ProcessIo::from_raw(frames, &spec, params, &inputs, &raw_outputs) }
            .with_position(position);
        processor.process(&mut io)?;
        Ok(output_samples.into_iter().next().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_plugin_api::{NodeProvider, NodeSpec};

    /// Every spec the plugin registers, so no node can miss the checks.
    fn specs() -> Vec<NodeSpec> {
        struct Specs(Vec<NodeSpec>);

        impl Registrar for Specs {
            fn register_node(&mut self, provider: Box<dyn NodeProvider>) {
                self.0.push(provider.spec());
            }
        }

        let mut specs = Specs(Vec::new());
        StdPlugin.register(&mut specs);
        specs.0
    }

    #[test]
    fn every_type_id_is_registered() {
        let specs = specs();
        for type_id in [
            GENERATOR,
            AMPLIFIER,
            SUMMING_AMPLIFIER,
            DEVICE_OUTPUT,
            OSCILLOGRAPH,
        ] {
            assert!(
                specs.iter().any(|spec| spec.type_id == type_id),
                "{type_id}"
            );
        }
    }

    #[test]
    fn specs_are_valid_and_fully_translated() {
        for spec in specs() {
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
    fn every_signal_is_one_conductor() {
        for spec in specs() {
            for socket in spec.inputs.iter().chain(&spec.outputs) {
                assert_eq!(socket.channels, 1, "{} {}", spec.type_id, socket.name);
            }
        }
    }
}

//! [`AMPLIFIER`]: an amplifier stage.
//!
//! The output voltage is the input voltage times the gain (negative: inverting). With
//! clipping on (off by default), it stays within the supply voltage like a real stage.

use v_signal_nodes_plugin_api::{
    NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, NumberParam, ParamKey,
    PluginError, ProcessIo, TextKey,
};

use crate::{AMPLIFIER, clipping_specs, clipping_translations, output_limit};

/// The voltage gain, V/V.
const FACTOR: NumberParam = NumberParam("gain");

pub(crate) struct Amplifier;

impl NodeProvider for Amplifier {
    fn spec(&self) -> NodeSpec {
        let [clipping, supply] = clipping_specs();
        NodeSpec::new(AMPLIFIER, "Amplifier", NodeKind::Processor)
            .input(NodeSocket::signal_in("In"))
            .output(NodeSocket::signal_out("Out"))
            .param(FACTOR.spec("Gain", -100.0, 100.0, 1.0))
            .param(clipping)
            .param(supply)
            .translate(
                "ru",
                [
                    (TextKey::Name, "Усилитель"),
                    (TextKey::Input(0), "Вход"),
                    (TextKey::Output(0), "Выход"),
                    (FACTOR.name_key(), "Усиление"),
                ],
            )
            .translate("ru", clipping_translations())
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(AmplifierProcessor)
    }
}

struct AmplifierProcessor;

impl NodeProcessor for AmplifierProcessor {
    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        let gain = io.get(FACTOR)?;
        let limit = output_limit(io)?;
        let input = io.input(0);
        for (out, sample) in io.output(0).iter_mut().zip(input) {
            *out = (sample * gain).clamp(-limit, limit);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CLIPPING, SUPPLY};
    use v_signal_nodes_plugin_api::ParamValue;
    use v_signal_nodes_plugin_api::testing::Harness;

    /// The output for `input`; `supply` turns clipping on.
    fn amplified(gain: f32, supply: Option<f32>, input: &[f32]) -> Vec<f32> {
        let mut amplifier = Harness::new(&Amplifier).with(FACTOR, gain);
        if let Some(supply) = supply {
            amplifier.set(CLIPPING, true).set(SUPPLY, supply);
        }
        amplifier.run(&[input]).remove(0)
    }

    #[test]
    fn scales_the_input() {
        assert_eq!(amplified(0.5, None, &[1.0, -2.0]), [0.5, -1.0]);
    }

    #[test]
    fn a_negative_gain_inverts() {
        assert_eq!(amplified(-2.0, None, &[1.0, -2.0]), [-2.0, 4.0]);
    }

    #[test]
    fn nothing_limits_the_output_by_default() {
        assert_eq!(amplified(10.0, None, &[7.0, -7.0]), [70.0, -70.0]);
    }

    #[test]
    fn with_clipping_on_the_output_stops_at_the_supply() {
        let output = amplified(10.0, Some(5.0), &[1.0, -1.0, 0.25]);
        assert_eq!(output, [5.0, -5.0, 2.5]);
    }

    #[test]
    fn wrong_parameter_kinds_are_errors() {
        let mut amplifier = Harness::new(&Amplifier);
        amplifier.set_raw(FACTOR.0, ParamValue::Choice(0));
        let error = amplifier.process(1, &[&[1.0]]).unwrap_err();
        assert!(error.message().contains("'gain'"), "{error}");
    }
}

//! [`MIXER`]: a summing amplifier.
//!
//! The output voltage is the weighted sum of three input voltages, times the gain:
//! `gain · (w1·V1 + w2·V2 + w3·V3)`, sample by sample. An unconnected input is at 0 V.
//! With clipping on (off by default), the output stays within the supply voltage.

use v_signal_nodes_plugin_api::{
    NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, NumberParam, ParamKey,
    PluginError, ProcessIo, TextKey,
};

use crate::{MIXER, clipping_specs, clipping_translations, output_limit};

/// The gain after the sum, V/V.
const FACTOR: NumberParam = NumberParam("gain");
/// The weight of each input.
const WEIGHTS: [NumberParam; 3] = [
    NumberParam("weight1"),
    NumberParam("weight2"),
    NumberParam("weight3"),
];

pub(crate) struct Mixer;

impl NodeProvider for Mixer {
    fn spec(&self) -> NodeSpec {
        let weight = |index: usize, name: &str| WEIGHTS[index].spec(name, -100.0, 100.0, 1.0);
        let [clipping, supply] = clipping_specs();
        NodeSpec::new(MIXER, "Summing amplifier", NodeKind::Processor)
            .input(NodeSocket::signal_in("In 1"))
            .input(NodeSocket::signal_in("In 2"))
            .input(NodeSocket::signal_in("In 3"))
            .output(NodeSocket::signal_out("Out"))
            .param(FACTOR.spec("Gain", -100.0, 100.0, 1.0))
            .param(weight(0, "Weight 1").advanced())
            .param(weight(1, "Weight 2").advanced())
            .param(weight(2, "Weight 3").advanced())
            .param(clipping)
            .param(supply)
            .translate(
                "ru",
                [
                    (TextKey::Name, "Сумматор"),
                    (TextKey::Input(0), "Вход 1"),
                    (TextKey::Input(1), "Вход 2"),
                    (TextKey::Input(2), "Вход 3"),
                    (TextKey::Output(0), "Выход"),
                    (FACTOR.name_key(), "Усиление"),
                    (WEIGHTS[0].name_key(), "Вес 1"),
                    (WEIGHTS[1].name_key(), "Вес 2"),
                    (WEIGHTS[2].name_key(), "Вес 3"),
                ],
            )
            .translate("ru", clipping_translations())
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(MixerProcessor)
    }
}

struct MixerProcessor;

impl NodeProcessor for MixerProcessor {
    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        let gain = io.get(FACTOR)?;
        let limit = output_limit(io)?;
        let weights = [
            io.get(WEIGHTS[0])?,
            io.get(WEIGHTS[1])?,
            io.get(WEIGHTS[2])?,
        ];
        let inputs = [io.input(0), io.input(1), io.input(2)];
        for (frame, out) in io.output(0).iter_mut().enumerate() {
            let sum: f32 = inputs
                .iter()
                .zip(weights)
                .map(|(input, weight)| input[frame] * weight)
                .sum();
            *out = (sum * gain).clamp(-limit, limit);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{prepared, run};
    use v_signal_nodes_plugin_api::ParamValue;

    /// The output for `inputs`; `supply` turns clipping on.
    fn mixed(gain: f32, weights: [f32; 3], supply: Option<f32>, inputs: [&[f32]; 3]) -> Vec<f32> {
        let mut processor = prepared(&Mixer);
        let params = [
            ParamValue::Number(gain),
            ParamValue::Number(weights[0]),
            ParamValue::Number(weights[1]),
            ParamValue::Number(weights[2]),
            ParamValue::Toggle(supply.is_some()),
            ParamValue::Number(supply.unwrap_or(15.0)),
        ];
        run(
            &Mixer,
            processor.as_mut(),
            &params,
            &inputs,
            inputs[0].len(),
        )
        .unwrap()
    }

    #[test]
    fn sums_the_weighted_inputs() {
        let inputs: [&[f32]; 3] = [&[1.0, 0.0], &[0.5, 1.0], &[0.25, -1.0]];

        assert_eq!(mixed(1.0, [1.0; 3], None, inputs), [1.75, 0.0]);
        assert_eq!(mixed(2.0, [1.0, 0.0, -1.0], None, inputs), [1.5, 2.0]);
    }

    #[test]
    fn opposite_voltages_add_up_to_nothing() {
        let inputs: [&[f32]; 3] = [&[7.0, -3.5], &[-7.0, 3.5], &[0.0, 0.0]];

        assert_eq!(mixed(1.0, [1.0; 3], None, inputs), [0.0, 0.0]);
    }

    #[test]
    fn with_clipping_on_the_output_stops_at_the_supply() {
        let inputs: [&[f32]; 3] = [&[4.0, -4.0], &[4.0, -4.0], &[0.0, 0.0]];

        assert_eq!(mixed(1.0, [1.0; 3], Some(5.0), inputs), [5.0, -5.0]);
        assert_eq!(mixed(1.0, [1.0; 3], None, inputs), [8.0, -8.0]);
    }
}

//! Parameters the host reads itself, of nodes that meet a device: [`NodeKind::Input`]
//! and [`NodeKind::Output`] nodes.
//!
//! A sample is a voltage, while a device takes and gives numbers within ±1 (its full
//! scale). A node that meets one says how they map with [`FULL_SCALE`], and an input
//! node which of the device's channels it takes with [`DEVICE_CHANNEL`]. Both are
//! optional: without them 1 V is full scale and the channels start at the first. The
//! host applies them, so the node's processor has nothing to do.
//!
//! [`NodeKind::Input`]: crate::NodeKind::Input
//! [`NodeKind::Output`]: crate::NodeKind::Output

use crate::{ChoiceParam, NodeSpec, NumberParam, ParamKey, ParamValue};

/// The voltage of the device's full scale, in volts: what an output node delivers is
/// divided by it on the way to the device, what an input node gets is multiplied by
/// it. A number parameter; [`DEFAULT_FULL_SCALE`] without it.
pub const FULL_SCALE: NumberParam = NumberParam("full_scale");
/// [`FULL_SCALE`] of a node without the parameter (or with a value that can't be one,
/// not above zero).
pub const DEFAULT_FULL_SCALE: f32 = 1.0;
/// The device channel the first channel of an input node takes, as an option index:
/// option `i` is channel `i + 1`, and the node's next channels take the next ones. A
/// choice parameter; the first channel without it.
pub const DEVICE_CHANNEL: ChoiceParam = ChoiceParam("device_channel");

/// The [`FULL_SCALE`] of a node of type `spec` with `params`.
pub fn full_scale(spec: &NodeSpec, params: &[ParamValue]) -> f32 {
    read(spec, params, FULL_SCALE)
        .filter(|&volts| volts > 0.0 && volts.is_finite())
        .unwrap_or(DEFAULT_FULL_SCALE)
}

/// The [`DEVICE_CHANNEL`] of a node of type `spec` with `params`: the index of the
/// device channel its first channel takes.
pub fn device_channel(spec: &NodeSpec, params: &[ParamValue]) -> usize {
    read(spec, params, DEVICE_CHANNEL).unwrap_or(0)
}

fn read<K: ParamKey>(spec: &NodeSpec, params: &[ParamValue], key: K) -> Option<K::Value> {
    let index = spec.param_index(key.id())?;
    key.read(*params.get(index)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeKind, NodeSocket};

    #[test]
    fn nodes_without_the_parameters_get_the_defaults() {
        let plain =
            NodeSpec::new("t.out", "Out", NodeKind::Output).input(NodeSocket::signal_in("in"));
        assert_eq!(full_scale(&plain, &[]), 1.0);
        assert_eq!(device_channel(&plain, &[]), 0);

        let set = NodeSpec::new("t.in", "In", NodeKind::Input)
            .output(NodeSocket::signal_out("out"))
            .param(DEVICE_CHANNEL.spec("Channel", ["1", "2"], 0))
            .param(FULL_SCALE.spec("Full scale", 0.001, 100.0, 1.0));
        let params = [ParamValue::Choice(1), ParamValue::Number(10.0)];
        assert_eq!(full_scale(&set, &params), 10.0);
        assert_eq!(device_channel(&set, &params), 1);
        let zero = [ParamValue::Choice(1), ParamValue::Number(0.0)];
        assert_eq!(full_scale(&set, &zero), 1.0, "never divides by zero");
    }
}

//! [`DEVICE_OUTPUT`]: the output to the device.
//!
//! The host reads this node's input and sends it to the output device, where the
//! voltage of the full-scale parameter (1 V by default) is the device's full scale;
//! higher voltages clip there.

use v_signal_nodes_plugin_api::{
    NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, PluginError, ProcessIo, TextKey,
};

use crate::{DEVICE_OUTPUT, full_scale_spec, full_scale_translation};

pub(crate) struct DeviceOutput;

impl NodeProvider for DeviceOutput {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(DEVICE_OUTPUT, "Device output", NodeKind::Output)
            .input(NodeSocket::signal_in("In"))
            .param(full_scale_spec())
            .translate(
                "ru",
                [
                    (TextKey::Name, "Выход устройства"),
                    (TextKey::Input(0), "Вход"),
                    full_scale_translation(),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(DeviceOutputProcessor)
    }
}

/// Does nothing: the sink has no outputs to fill, and the host applies the full scale.
struct DeviceOutputProcessor;

impl NodeProcessor for DeviceOutputProcessor {
    fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

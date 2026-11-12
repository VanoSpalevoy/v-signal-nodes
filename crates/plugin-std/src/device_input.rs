//! [`DEVICE_INPUT`]: the input from the device.
//!
//! The host fills this node's output with what the input device takes in on the
//! chosen channel, as a voltage: the device's full scale is the voltage of the
//! full-scale parameter (1 V by default). Silence when no input device runs, as in a
//! one-pass run.

use v_signal_nodes_plugin_api::{
    DEVICE_CHANNEL, NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, ParamKey,
    PluginError, ProcessIo, TextKey,
};

use crate::{DEVICE_INPUT, full_scale_spec, full_scale_translation};

/// Options of [`DEVICE_CHANNEL`]: the device's first channels.
const CHANNELS: [&str; 8] = ["1", "2", "3", "4", "5", "6", "7", "8"];

pub(crate) struct DeviceInput;

impl NodeProvider for DeviceInput {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(DEVICE_INPUT, "Device input", NodeKind::Input)
            .output(NodeSocket::signal_out("Out"))
            .param(DEVICE_CHANNEL.spec("Channel", CHANNELS, 0))
            .param(full_scale_spec())
            .translate(
                "ru",
                [
                    (TextKey::Name, "Вход устройства"),
                    (TextKey::Output(0), "Выход"),
                    (DEVICE_CHANNEL.name_key(), "Канал"),
                    full_scale_translation(),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(DeviceInputProcessor)
    }
}

/// Does nothing: the host fills the output, at the full scale it reads itself.
struct DeviceInputProcessor;

impl NodeProcessor for DeviceInputProcessor {
    fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

//! A stub plugin with one node type that does nothing (`stub.thru`).
//!
//! It exists to exercise the whole path a plugin node takes: the spec is
//! registered in `core` through the plugin host, a `core::Node` is built from it,
//! and the editor turns that node into a canvas widget. The processor leaves its
//! output silent.

use v_signal_nodes_plugin_api::{
    ChoiceParam, NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, NumberParam,
    ParamKey, Plugin, PluginError, ProcessIo, Registrar, Rgb, TextKey, ToggleParam,
};

/// Type id of the stub node.
pub const STUB_NODE: &str = "stub.thru";

/// The stub plugin, named `"stub"`.
pub struct StubPlugin;

impl Plugin for StubPlugin {
    fn name(&self) -> &str {
        "stub"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(&self, registrar: &mut dyn Registrar) {
        registrar.register_node(Box::new(StubNode));
    }
}

/// [`STUB_NODE`]: one input, one output and one parameter of each kind, so the
/// editor has every kind of label to show. Gray, its own color, so it doesn't
/// pass for a real processor.
struct StubNode;

const LEVEL: NumberParam = NumberParam("level");
const MODE: ChoiceParam = ChoiceParam("mode");
const BYPASS: ToggleParam = ToggleParam("bypass");

impl NodeProvider for StubNode {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(STUB_NODE, "Stub", NodeKind::Processor)
            .color(Rgb::new(160, 164, 172))
            .input(NodeSocket::audio_in("In"))
            .output(NodeSocket::audio_out("Out"))
            .param(LEVEL.spec("Level", 0.0, 1.0, 0.5))
            .param(MODE.spec("Mode", ["idle", "busy"], 0))
            .param(BYPASS.spec("Bypass", true))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Заглушка"),
                    (TextKey::Input(0), "Вход"),
                    (TextKey::Output(0), "Выход"),
                    (LEVEL.name_key(), "Уровень"),
                    (MODE.name_key(), "Режим"),
                    (MODE.option_key(0), "простой"),
                    (MODE.option_key(1), "занят"),
                    (BYPASS.name_key(), "Обход"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(StubProcessor)
    }
}

/// Does nothing: outputs start zeroed and stay that way.
struct StubProcessor;

impl NodeProcessor for StubProcessor {
    fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_spec_is_valid() {
        assert!(StubNode.spec().validate().is_ok());
    }
}

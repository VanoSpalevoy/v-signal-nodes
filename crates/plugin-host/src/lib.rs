//! Loads plugins and builds nodes from the node types they provide.
//!
//! [`PluginHost::load`] collects a plugin's [`NodeProvider`]s, registers their
//! specs in a [`NodeFactory`] and keeps the providers by type id.
//! [`PluginHost::build_node`] then returns the [`Node`] together with its
//! [`NodeProcessor`].
//!
//! Plugins built as a `cdylib` with `export_plugin!` are loaded with
//! [`PluginHost::load_library`] (see [`DynamicPlugin`]).

use std::collections::HashMap;
use std::ffi::OsStr;

use v_signal_nodes_core::{AppError, Node, NodeFactory, NodeSpec};
use v_signal_nodes_plugin_api::{NodeProcessor, NodeProvider, Plugin, Registrar};

mod dynamic;

pub use dynamic::DynamicPlugin;

/// A node built from a plugin: the graph-side description plus its DSP state.
pub struct PluginNode {
    pub node: Node,
    pub processor: Box<dyn NodeProcessor>,
}

/// Registry of loaded plugins and their node types.
#[derive(Default)]
pub struct PluginHost {
    factory: NodeFactory,
    /// Providers by node type id.
    providers: HashMap<String, Box<dyn NodeProvider>>,
    /// Names of loaded plugins, in load order.
    plugins: Vec<String>,
    /// Index into `plugins` of the plugin that registered each node type id.
    owners: HashMap<String, usize>,
}

/// [`Registrar`] that only collects providers, so a plugin is registered all or
/// nothing.
#[derive(Default)]
struct Collector(Vec<Box<dyn NodeProvider>>);

impl Registrar for Collector {
    fn register_node(&mut self, provider: Box<dyn NodeProvider>) {
        self.0.push(provider);
    }
}

impl PluginHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads `plugin` and registers all its node types. Fails without changing
    /// anything if the plugin is already loaded or any of its node specs is
    /// invalid or clashes with a registered type.
    ///
    /// There's no API version check here: a linked plugin is compiled against
    /// this host's `plugin-api`, and a library's version is checked when it's
    /// opened ([`DynamicPlugin::open`]).
    pub fn load(&mut self, plugin: &dyn Plugin) -> Result<(), AppError> {
        let name = plugin.name();
        if self.plugins.iter().any(|loaded| loaded == name) {
            return Err(AppError::InvalidInput(format!(
                "plugin '{name}' is already loaded"
            )));
        }

        let mut collector = Collector::default();
        plugin.register(&mut collector);

        // Validate everything on a scratch factory first, so a bad plugin leaves
        // no partial registration behind.
        let mut staged = NodeFactory::new();
        let mut entries = Vec::with_capacity(collector.0.len());
        for provider in collector.0 {
            let spec = provider.spec();
            if self.factory.spec(&spec.type_id).is_some() {
                return Err(AppError::InvalidInput(format!(
                    "plugin '{name}': node type '{}' is already registered",
                    spec.type_id
                )));
            }
            staged.register(spec.clone())?;
            entries.push((spec, provider));
        }

        for (spec, provider) in entries {
            self.owners.insert(spec.type_id.clone(), self.plugins.len());
            self.providers.insert(spec.type_id.clone(), provider);
            self.factory.register(spec)?;
        }
        self.plugins.push(name.to_owned());
        Ok(())
    }

    /// Loads the plugin library at `path` (a `cdylib` built with
    /// `export_plugin!`) and registers its node types like [`load`](Self::load).
    /// The library stays loaded while any of its providers or processors exist.
    ///
    /// # Safety
    ///
    /// Same as [`DynamicPlugin::open`]: the library must be a trusted plugin.
    pub unsafe fn load_library(&mut self, path: impl AsRef<OsStr>) -> Result<(), AppError> {
        // SAFETY: forwarded to the caller.
        let plugin = unsafe { DynamicPlugin::open(path) }?;
        self.load(&plugin)
    }

    /// Names of the loaded plugins, in load order.
    pub fn plugins(&self) -> &[String] {
        &self.plugins
    }

    /// Name of the plugin that registered node type `type_id`, if it is loaded.
    pub fn plugin_of(&self, type_id: &str) -> Option<&str> {
        self.owners
            .get(type_id)
            .map(|&index| self.plugins[index].as_str())
    }

    /// All available node types, sorted by type id.
    pub fn specs(&self) -> impl Iterator<Item = &NodeSpec> {
        self.factory.specs()
    }

    /// Builds node `id` of type `type_id` with a fresh processor.
    pub fn build_node(&self, type_id: &str, id: usize) -> Result<PluginNode, AppError> {
        let node = self.factory.build_node(type_id, id)?;
        let processor = self.providers[type_id].create_processor();
        Ok(PluginNode { node, processor })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::{NodeKind, NodeSocket};
    use v_signal_nodes_core::{ParamSpec, ParamValue};
    use v_signal_nodes_plugin_api::{PluginError, ProcessIo, RawBuffer, ffi};

    struct Gain;

    /// Multiplies by the `gain` parameter. Fails if not prepared, and adds an
    /// offset that `reset` clears, so the tests see every call go through.
    struct GainProcessor {
        prepared: bool,
        offset: f32,
    }

    impl NodeProcessor for GainProcessor {
        fn prepare(&mut self, _sample_rate: f32, _max_frames: usize) -> Result<(), PluginError> {
            self.prepared = true;
            Ok(())
        }

        fn reset(&mut self) {
            self.offset = 0.0;
        }

        fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
            if !self.prepared {
                return Err(PluginError::new("not prepared"));
            }
            let factor = io.number(0);
            let input = io.input(0);
            for (out, sample) in io.output(0).iter_mut().zip(input) {
                *out = sample * factor + self.offset;
            }
            Ok(())
        }
    }

    impl NodeProvider for Gain {
        fn spec(&self) -> NodeSpec {
            NodeSpec::new("test.gain", "Gain", NodeKind::Processor)
                .input(NodeSocket::audio_in("in"))
                .output(NodeSocket::audio_out("out"))
                .param(ParamSpec::number("gain", "Gain", 0.0, 4.0, 2.0))
        }

        fn create_processor(&self) -> Box<dyn NodeProcessor> {
            Box::new(GainProcessor {
                prepared: false,
                offset: 1.0,
            })
        }
    }

    struct TestPlugin;

    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            "test"
        }

        fn register(&self, registrar: &mut dyn Registrar) {
            registrar.register_node(Box::new(Gain));
        }
    }

    /// Runs a one-input, one-output processor over `input`.
    fn run(
        processor: &mut dyn NodeProcessor,
        params: &[ParamValue],
        input: &[f32],
    ) -> Result<Vec<f32>, PluginError> {
        let mut output = vec![0.0; input.len()];
        let inputs = [RawBuffer::input(input)];
        let outputs = [RawBuffer::output(&mut output)];
        // SAFETY: both buffers are `input.len()` long, live for the call and
        // don't overlap.
        let mut io = unsafe { ProcessIo::from_raw(input.len(), params, &inputs, &outputs) };
        processor.process(&mut io)?;
        Ok(output)
    }

    /// Drives `processor` through its whole life cycle.
    fn check_life_cycle(processor: &mut dyn NodeProcessor) {
        let params = [ParamValue::Number(2.0)];
        assert_eq!(
            run(processor, &params, &[1.0]).unwrap_err().message(),
            "not prepared"
        );
        processor.prepare(48_000.0, 64).unwrap();
        assert_eq!(run(processor, &params, &[1.0, -0.5]).unwrap(), [3.0, 0.0]);
        processor.reset();
        assert_eq!(run(processor, &params, &[1.0, -0.5]).unwrap(), [2.0, -1.0]);
        let half = [ParamValue::Number(0.5)];
        assert_eq!(run(processor, &half, &[1.0]).unwrap(), [0.5]);
    }

    #[test]
    fn builds_a_working_node_from_a_plugin() {
        let mut host = PluginHost::new();
        host.load(&TestPlugin).unwrap();

        let mut built = host.build_node("test.gain", 3).unwrap();

        assert_eq!(built.node.id, 3);
        assert_eq!(built.node.type_id, "test.gain");
        check_life_cycle(built.processor.as_mut());
        assert_eq!(host.plugins(), ["test"]);
        assert_eq!(host.plugin_of("test.gain"), Some("test"));
        assert_eq!(host.plugin_of("test.missing"), None);
    }

    #[test]
    fn builds_a_working_node_through_the_c_abi() {
        // SAFETY: `export` produces a plugin object following the `ffi` protocol,
        // and its code is part of this binary.
        let plugin = unsafe { DynamicPlugin::from_ffi(ffi::export(|| TestPlugin), None) }.unwrap();
        let mut host = PluginHost::new();
        host.load(&plugin).unwrap();
        drop(plugin);

        let mut built = host.build_node("test.gain", 0).unwrap();

        check_life_cycle(built.processor.as_mut());
        assert_eq!(host.plugins(), ["test"]);
    }

    #[test]
    fn specs_survive_the_c_abi() {
        // SAFETY: as in `builds_a_working_node_through_the_c_abi`.
        let plugin = unsafe {
            DynamicPlugin::from_ffi(ffi::export(|| v_signal_nodes_plugin_std::StdPlugin), None)
        }
        .unwrap();
        let mut through_ffi = PluginHost::new();
        through_ffi.load(&plugin).unwrap();
        let mut linked = PluginHost::new();
        linked.load(&v_signal_nodes_plugin_std::StdPlugin).unwrap();

        assert!(linked.specs().eq(through_ffi.specs()));
    }

    #[test]
    fn rejects_a_missing_library() {
        let mut host = PluginHost::new();

        // SAFETY: the path doesn't exist, so nothing is loaded.
        assert!(unsafe { host.load_library("no-such-plugin-library") }.is_err());
        assert!(host.plugins().is_empty());
    }

    #[test]
    fn rejects_loading_twice_and_unknown_types() {
        let mut host = PluginHost::new();
        host.load(&TestPlugin).unwrap();

        assert!(host.load(&TestPlugin).is_err());
        assert!(host.build_node("test.missing", 0).is_err());
    }
}

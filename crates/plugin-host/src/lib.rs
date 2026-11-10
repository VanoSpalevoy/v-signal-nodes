//! Loads plugins and builds nodes from the node types they provide.
//!
//! [`PluginHost::load`] collects a plugin's [`NodeProvider`]s and keeps each with its
//! validated spec by type id, in one registry. [`PluginHost::build_node`] then
//! returns a [`Node`] for the graph, and
//! [`PluginHost::create_processor`] a [`NodeProcessor`] for running it: separately,
//! because nodes are edited long before anything runs, and only the nodes of a
//! compiled plan need a processor.
//!
//! [`PluginHost::plugins`] lists the loaded plugins with their versions
//! ([`LoadedPlugin`]).
//!
//! Plugins built as a `cdylib` with `export_plugin!` are loaded with
//! [`PluginHost::load_library`] (see [`DynamicPlugin`]).
//!
//! A linked plugin panicking in any call the host makes (loading it, creating a
//! processor) is an error, not a crash, like a panic in a dynamic library, which is
//! caught on the library's side.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use v_signal_nodes_core::{Error, Node, NodeSpec};
use v_signal_nodes_plugin_api::{NodeProcessor, NodeProvider, Plugin, Registrar};

mod dynamic;

pub use dynamic::DynamicPlugin;

/// Registry of loaded plugins and their node types.
#[derive(Default)]
pub struct PluginHost {
    /// Node types by type id (sorted, so listings are stable).
    types: BTreeMap<String, NodeType>,
    /// Loaded plugins, in load order.
    plugins: Vec<LoadedPlugin>,
}

/// A loaded plugin's name and version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedPlugin {
    pub name: String,
    /// The plugin's own version (`Plugin::version`).
    pub version: String,
}

/// A registered node type: its spec, its provider and who registered it.
struct NodeType {
    /// The validated spec, shared with every node built from it.
    spec: Arc<NodeSpec>,
    provider: Box<dyn NodeProvider>,
    /// Index into [`PluginHost::plugins`] of the plugin that registered it.
    plugin: usize,
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
    /// anything if the plugin is already loaded, panics, or any of its node specs
    /// is invalid or clashes with a registered type.
    ///
    /// There's no API version check here: a linked plugin is compiled against
    /// this host's `plugin-api`, and a library's version is checked when it's
    /// opened ([`DynamicPlugin::open`]).
    pub fn load(&mut self, plugin: &dyn Plugin) -> Result<(), Error> {
        let (name, version) = guarded(
            || (plugin.name().to_owned(), plugin.version().to_owned()),
            || "a plugin panicked telling its name or version".to_owned(),
        )?;
        if self.plugin(&name).is_some() {
            return Err(Error::PluginAlreadyLoaded(name));
        }

        let mut collector = Collector::default();
        guarded(
            || plugin.register(&mut collector),
            || format!("{name} panicked registering its node types"),
        )?;
        let providers = collector.0;
        let specs = guarded(
            || {
                providers
                    .iter()
                    .map(|provider| provider.spec())
                    .collect::<Vec<_>>()
            },
            || format!("{name} panicked describing its node types"),
        )?;

        // Check everything before registering anything, so a bad plugin leaves no
        // partial registration behind.
        let mut type_ids = HashSet::new();
        for spec in &specs {
            spec.validate()?;
            let type_id = &spec.type_id;
            if self.types.contains_key(type_id) || !type_ids.insert(type_id) {
                return Err(Error::DuplicateNodeType(type_id.clone()));
            }
        }

        let index = self.plugins.len();
        let types = specs.into_iter().zip(providers).map(|(spec, provider)| {
            let node_type = NodeType {
                spec: Arc::new(spec),
                provider,
                plugin: index,
            };
            (node_type.spec.type_id.clone(), node_type)
        });
        self.types.extend(types);
        self.plugins.push(LoadedPlugin { name, version });
        Ok(())
    }

    /// Loads the plugin library at `path` (a `cdylib` built with
    /// `export_plugin!`) and registers its node types like [`load`](Self::load).
    /// The library stays loaded while any of its providers or processors exist.
    ///
    /// # Safety
    ///
    /// Same as [`DynamicPlugin::open`]: the library must be a trusted plugin.
    pub unsafe fn load_library(&mut self, path: impl AsRef<OsStr>) -> Result<(), Error> {
        // SAFETY: forwarded to the caller.
        let plugin = unsafe { DynamicPlugin::open(path) }?;
        self.load(&plugin)
    }

    /// The loaded plugins, in load order.
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// The loaded plugin named `name`.
    pub fn plugin(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|plugin| plugin.name == name)
    }

    /// The plugin that registered node type `type_id`, if it is loaded.
    pub fn plugin_of(&self, type_id: &str) -> Option<&LoadedPlugin> {
        self.types
            .get(type_id)
            .map(|node_type| &self.plugins[node_type.plugin])
    }

    /// All available node types, sorted by type id. The specs are shared with
    /// the nodes built from them; clone the [`Arc`] to keep one.
    pub fn specs(&self) -> impl Iterator<Item = &Arc<NodeSpec>> {
        self.types.values().map(|node_type| &node_type.spec)
    }

    /// The spec of node type `type_id`, if a loaded plugin has it.
    pub fn spec(&self, type_id: &str) -> Option<&Arc<NodeSpec>> {
        self.types.get(type_id).map(|node_type| &node_type.spec)
    }

    /// Builds a node of type `type_id`, with every parameter at its default.
    pub fn build_node(&self, type_id: &str) -> Result<Node, Error> {
        Ok(Node::new(Arc::clone(&self.node_type(type_id)?.spec)))
    }

    /// Creates a fresh processor for a node of type `type_id`. A plugin panicking in
    /// there is an error.
    pub fn create_processor(&self, type_id: &str) -> Result<Box<dyn NodeProcessor>, Error> {
        let provider = &self.node_type(type_id)?.provider;
        guarded(
            || provider.create_processor(),
            || format!("{type_id} panicked creating its processor"),
        )
    }

    /// The registered node type `type_id`.
    fn node_type(&self, type_id: &str) -> Result<&NodeType, Error> {
        self.types
            .get(type_id)
            .ok_or_else(|| Error::UnknownNodeType(type_id.to_owned()))
    }
}

/// Runs `call`, a call into a plugin: a panic in it is an error, described by
/// `what`, instead of a crash.
fn guarded<T>(call: impl FnOnce() -> T, what: impl FnOnce() -> String) -> Result<T, Error> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|_| Error::Plugin(what()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::{
        ChoiceParam, NodeKind, NodeSocket, NumberParam, ParamValue, PlotWindow, ToggleParam, Unit,
    };
    use v_signal_nodes_plugin_api::testing::Harness;
    use v_signal_nodes_plugin_api::{PlotTrace, PluginError, ProcessIo, ffi};

    const GAIN: NumberParam = NumberParam("gain");

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
            let factor = io.get(GAIN)?;
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
                .input(NodeSocket::signal_in("in"))
                .output(NodeSocket::signal_out("out"))
                .param(GAIN.spec("Gain", 0.0, 4.0, 2.0))
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

        fn version(&self) -> &str {
            "1.2.3"
        }

        fn register(&self, registrar: &mut dyn Registrar) {
            registrar.register_node(Box::new(Gain));
        }
    }

    /// Drives `processor`, one of a [`Gain`] node, through its whole life cycle.
    fn check_life_cycle(processor: Box<dyn NodeProcessor>) {
        let mut gain = Harness::from_parts(Gain.spec(), processor).without_prepare();
        let error = gain.process(1, &[&[1.0]]).unwrap_err();
        assert_eq!(error.message(), "not prepared");
        gain.prepare().unwrap();
        assert_eq!(gain.run(&[&[1.0, -0.5]]), [[3.0, 0.0]]);
        gain.reset();
        assert_eq!(gain.run(&[&[1.0, -0.5]]), [[2.0, -1.0]]);
        assert_eq!(gain.set(GAIN, 0.5).run(&[&[1.0]]), [[0.5]]);
        // A value of the wrong kind is an error, not a silent 0.0.
        gain.set_raw(GAIN.0, ParamValue::Toggle(true));
        assert!(gain.process(1, &[&[1.0]]).is_err());
    }

    /// `test`, version 1.2.3, as the host lists it.
    fn test_plugin() -> LoadedPlugin {
        LoadedPlugin {
            name: "test".into(),
            version: "1.2.3".into(),
        }
    }

    #[test]
    fn builds_a_working_node_from_a_plugin() {
        let mut host = PluginHost::new();
        host.load(&TestPlugin).unwrap();

        let node = host.build_node("test.gain").unwrap();
        let processor = host.create_processor("test.gain").unwrap();

        assert_eq!(node.type_id(), "test.gain");
        assert!(Arc::ptr_eq(node.spec(), host.spec("test.gain").unwrap()));
        check_life_cycle(processor);
        assert_eq!(host.plugins(), [test_plugin()]);
        assert_eq!(host.plugin("test"), Some(&test_plugin()));
        assert_eq!(host.plugin_of("test.gain"), Some(&test_plugin()));
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

        let processor = host.create_processor("test.gain").unwrap();

        check_life_cycle(processor);
        assert_eq!(host.plugins(), [test_plugin()]);
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
        assert_eq!(linked.plugins(), through_ffi.plugins());
        assert!(
            through_ffi
                .specs()
                .any(|spec| spec.params.iter().any(|param| param.advanced)),
            "the comparison covers advanced parameters"
        );
        for unit in [Unit::Hertz, Unit::Volts, Unit::Degrees] {
            assert!(
                through_ffi
                    .specs()
                    .any(|spec| spec.params.iter().any(|param| param.unit == Some(unit))),
                "the comparison covers parameters in {unit:?}"
            );
        }
    }

    #[test]
    fn plots_cross_the_c_abi() {
        // SAFETY: as in `builds_a_working_node_through_the_c_abi`.
        let plugin = unsafe {
            DynamicPlugin::from_ffi(ffi::export(|| v_signal_nodes_plugin_std::StdPlugin), None)
        }
        .unwrap();
        let mut host = PluginHost::new();
        host.load(&plugin).unwrap();
        let oscilloscope = v_signal_nodes_plugin_std::DUAL_BEAM_OSCILLOSCOPE;
        let spec = NodeSpec::clone(host.spec(oscilloscope).unwrap());
        let processor = host.create_processor(oscilloscope).unwrap();
        // A sweep of 10 samples, free run, so the block holds one whole; interpolated.
        let mut oscilloscope = Harness::from_parts(spec, processor)
            .sample_rate(1000.0)
            .with(NumberParam("sweep"), 10.0)
            .with(ChoiceParam("trigger"), 0)
            .with(ToggleParam("interpolation"), true);

        oscilloscope.run(&[&[0.5; 10], &[-2.0; 10]]);

        let plotted = oscilloscope.plotted(PlotWindow("screen"));
        assert_eq!(plotted.len(), 1);
        assert!(
            (plotted[0].x_step - 0.001).abs() < 1e-6,
            "{}",
            plotted[0].x_step
        );
        assert_eq!(plotted[0].beams, [[0.5; 10], [-2.0; 10]]);
        assert_eq!(plotted[0].trace, Some(PlotTrace::Interpolated));
    }

    #[test]
    fn colors_and_channels_survive_the_c_abi() {
        struct Wide;

        impl NodeProvider for Wide {
            fn spec(&self) -> NodeSpec {
                NodeSpec::new("test.wide", "Wide", NodeKind::Generator)
                    .color(v_signal_nodes_core::Rgb::new(1, 2, 3))
                    .input(NodeSocket::control_in("rate").with_channels(3))
                    .output(NodeSocket::signal_out("out").with_channels(2))
            }

            fn create_processor(&self) -> Box<dyn NodeProcessor> {
                Box::new(GainProcessor {
                    prepared: false,
                    offset: 0.0,
                })
            }
        }

        struct WidePlugin;

        impl Plugin for WidePlugin {
            fn name(&self) -> &str {
                "wide"
            }

            fn version(&self) -> &str {
                "0.1.0"
            }

            fn register(&self, registrar: &mut dyn Registrar) {
                registrar.register_node(Box::new(Wide));
            }
        }

        // SAFETY: as in `builds_a_working_node_through_the_c_abi`.
        let plugin = unsafe { DynamicPlugin::from_ffi(ffi::export(|| WidePlugin), None) }.unwrap();
        let mut host = PluginHost::new();
        host.load(&plugin).unwrap();

        assert_eq!(**host.spec("test.wide").unwrap(), Wide.spec());
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

        assert!(matches!(
            host.load(&TestPlugin),
            Err(Error::PluginAlreadyLoaded(_))
        ));
        assert!(matches!(
            host.build_node("test.missing"),
            Err(Error::UnknownNodeType(_))
        ));
        assert!(matches!(
            host.create_processor("test.missing"),
            Err(Error::UnknownNodeType(_))
        ));
    }

    /// Where [`Configurable`] panics.
    #[derive(Clone, Copy, PartialEq)]
    enum Panic {
        Nowhere,
        Name,
        Register,
        Spec,
    }

    /// A plugin with a [`Gain`] under each of `type_ids`, panicking in `panic`.
    struct Configurable {
        type_ids: &'static [&'static str],
        panic: Panic,
    }

    /// [`Gain`] under another type id, panicking in `spec` if `panics`.
    struct Renamed {
        type_id: &'static str,
        panics: bool,
    }

    impl NodeProvider for Renamed {
        fn spec(&self) -> NodeSpec {
            assert!(!self.panics, "test panic in spec");
            NodeSpec {
                type_id: self.type_id.into(),
                ..Gain.spec()
            }
        }

        fn create_processor(&self) -> Box<dyn NodeProcessor> {
            Gain.create_processor()
        }
    }

    impl Plugin for Configurable {
        fn name(&self) -> &str {
            assert!(self.panic != Panic::Name, "test panic in name");
            "configurable"
        }

        fn version(&self) -> &str {
            "0.1.0"
        }

        fn register(&self, registrar: &mut dyn Registrar) {
            assert!(self.panic != Panic::Register, "test panic in register");
            for &type_id in self.type_ids {
                registrar.register_node(Box::new(Renamed {
                    type_id,
                    panics: self.panic == Panic::Spec,
                }));
            }
        }
    }

    #[test]
    fn a_plugin_with_a_taken_type_id_is_refused_whole() {
        let mut host = PluginHost::new();
        host.load(&TestPlugin).unwrap();

        for type_ids in [
            &["test.fresh", "test.gain"][..],
            &["test.twice", "test.twice"],
        ] {
            let plugin = Configurable {
                type_ids,
                panic: Panic::Nowhere,
            };
            assert!(matches!(
                host.load(&plugin),
                Err(Error::DuplicateNodeType(_))
            ));
        }

        assert_eq!(host.plugins(), [test_plugin()]);
        assert_eq!(host.specs().count(), 1);
        let plugin = Configurable {
            type_ids: &["test.fresh"],
            panic: Panic::Nowhere,
        };
        host.load(&plugin).unwrap();
        assert_eq!(host.plugin_of("test.fresh").unwrap().name, "configurable");
    }

    #[test]
    fn a_panic_loading_a_plugin_is_an_error() {
        let mut host = PluginHost::new();

        for panic in [Panic::Name, Panic::Register, Panic::Spec] {
            let plugin = Configurable {
                type_ids: &["test.fresh"],
                panic,
            };
            assert!(matches!(host.load(&plugin), Err(Error::Plugin(_))));
        }

        assert!(host.plugins().is_empty());
        assert_eq!(host.specs().count(), 0);
        host.load(&TestPlugin).unwrap();
    }

    #[test]
    fn a_panic_creating_a_processor_is_an_error() {
        struct Panics;

        impl NodeProvider for Panics {
            fn spec(&self) -> NodeSpec {
                Gain.spec()
            }

            fn create_processor(&self) -> Box<dyn NodeProcessor> {
                panic!("test panic in create_processor");
            }
        }

        struct PanicsPlugin;

        impl Plugin for PanicsPlugin {
            fn name(&self) -> &str {
                "panics"
            }

            fn version(&self) -> &str {
                "0.1.0"
            }

            fn register(&self, registrar: &mut dyn Registrar) {
                registrar.register_node(Box::new(Panics));
            }
        }

        let mut host = PluginHost::new();
        host.load(&PanicsPlugin).unwrap();

        assert!(matches!(
            host.create_processor("test.gain"),
            Err(Error::Plugin(_))
        ));
    }
}

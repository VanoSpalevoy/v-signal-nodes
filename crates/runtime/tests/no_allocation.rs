//! The real-time rule itself: no allocation or freeing on the real-time thread, also
//! while a new plan and parameter messages arrive. Has its own binary because it
//! replaces the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use v_signal_nodes_core::{
    Error, Graph, InputRef, NodeId, NodeKind, NodeSocket, NodeSpec, OutputRef, ParamValue, Step,
    compile,
};
use v_signal_nodes_plugin_api::{
    NodeProcessor, NodeProvider, Plugin, PluginError, ProcessIo, Registrar,
};
use v_signal_nodes_plugin_host::PluginHost;
use v_signal_nodes_plugin_std::{
    AMPLIFIER, DEVICE_OUTPUT, GENERATOR, OSCILLOSCOPE, SUMMING_AMPLIFIER, StdPlugin,
};
use v_signal_nodes_runtime::{plots, realtime};

/// Counts the calls made while [`COUNTING`] is on, on this thread only.
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    // `try_with`: the allocator may run while the thread is shutting down.
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = CALLS.try_with(|calls| calls.set(calls.get() + 1));
    }
}

// SAFETY: forwards everything to `System`; counting touches only const-initialized
// thread locals, which don't allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forwarded to the caller.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count();
        // SAFETY: forwarded to the caller.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: forwarded to the caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// How many allocator calls `work` makes.
fn allocator_calls(work: impl FnOnce()) -> usize {
    CALLS.set(0);
    COUNTING.set(true);
    work();
    COUNTING.set(false);
    CALLS.get()
}

/// Type id of [`StereoSink`].
const STEREO_SINK: &str = "test.stereo_sink";

/// An output node with a stereo input, which a mono signal goes through a conversion
/// for: the standard nodes are all mono.
struct StereoSink;

impl NodeProvider for StereoSink {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(STEREO_SINK, "Stereo sink", NodeKind::Output)
            .input(NodeSocket::signal_in("in").with_channels(2))
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(StereoSinkProcessor)
    }
}

struct StereoSinkProcessor;

impl NodeProcessor for StereoSinkProcessor {
    fn process(&mut self, _io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

struct StereoSinkPlugin;

impl Plugin for StereoSinkPlugin {
    fn name(&self) -> &str {
        "test"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn register(&self, registrar: &mut dyn Registrar) {
        registrar.register_node(Box::new(StereoSink));
    }
}

/// A processor for `step`, as the editor makes them: with plots if it has windows.
fn make_processor(host: &PluginHost, step: &Step) -> Result<Box<dyn NodeProcessor>, Error> {
    let processor = host.create_processor(&step.spec.type_id)?;
    Ok(plots::with_plots(processor, step.spec.windows.len()).0)
}

fn add(graph: &mut Graph, host: &PluginHost, type_id: &str) -> NodeId {
    graph.add(host.build_node(type_id).unwrap())
}

fn connect(graph: &mut Graph, from: NodeId, to: NodeId, input: usize) {
    graph
        .connect(OutputRef::new(from, 0), InputRef::new(to, input))
        .unwrap();
}

#[test]
fn the_real_time_thread_doesnt_allocate() {
    let mut host = PluginHost::new();
    host.load(&StdPlugin).unwrap();
    host.load(&StereoSinkPlugin).unwrap();
    let mut graph = Graph::new();
    let source = add(&mut graph, &host, GENERATOR);
    let gain = add(&mut graph, &host, AMPLIFIER);
    let mixer = add(&mut graph, &host, SUMMING_AMPLIFIER);
    let sink = add(&mut graph, &host, DEVICE_OUTPUT);
    let direct = add(&mut graph, &host, STEREO_SINK);
    let scope = add(&mut graph, &host, OSCILLOSCOPE);
    connect(&mut graph, source, gain, 0);
    connect(&mut graph, gain, mixer, 0);
    connect(&mut graph, source, mixer, 1);
    connect(&mut graph, mixer, sink, 0);
    // Mono into a stereo input: goes through a conversion.
    connect(&mut graph, source, direct, 0);
    // Publishes sweeps for its window: the device block is longer than a sweep.
    connect(&mut graph, mixer, scope, 0);
    let factor = graph
        .node(gain)
        .unwrap()
        .spec()
        .param_index("gain")
        .unwrap();

    let (mut controller, mut renderer) = realtime::channel(48_000.0, 64);
    let make = |step: &Step| make_processor(&host, step);
    let mut device = vec![0.0; 2048];

    controller.run_plan(compile(&graph).unwrap(), make).unwrap();
    let first_plan = allocator_calls(|| renderer.render(&mut device, 2));
    assert_eq!(first_plan, 0, "applying the first plan and rendering");
    assert!(device.iter().any(|&sample| sample != 0.0));

    let rendering = allocator_calls(|| renderer.render(&mut device, 2));
    assert_eq!(rendering, 0, "rendering");

    assert!(controller.set_param(gain, factor, ParamValue::Number(0.5)));
    assert!(controller.reset());
    let messages = allocator_calls(|| renderer.render(&mut device, 2));
    assert_eq!(messages, 0, "parameter and reset messages");

    // A changed graph: a new plan with one new node, the rest carried over.
    let extra = add(&mut graph, &host, DEVICE_OUTPUT);
    connect(&mut graph, gain, extra, 0);
    controller.run_plan(compile(&graph).unwrap(), make).unwrap();
    let replacing = allocator_calls(|| renderer.render(&mut device, 2));
    assert_eq!(replacing, 0, "replacing the plan");
    assert!(controller.poll().is_none());

    controller.clear().unwrap();
    let clearing = allocator_calls(|| renderer.render(&mut device, 2));
    assert_eq!(clearing, 0, "clearing");
}

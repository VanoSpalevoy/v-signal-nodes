//! The real-time rule for the device side: once made, a [`Resampled`] output and a
//! [`DeviceInput`] neither allocate nor free on the device's thread. Has its own
//! binary because it replaces the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use v_signal_nodes_core::{Graph, InputRef, OutputRef, compile};
use v_signal_nodes_device::{DeviceInput, Resampled};
use v_signal_nodes_plugin_host::PluginHost;
use v_signal_nodes_plugin_std::{DEVICE_OUTPUT, GENERATOR, StdPlugin};
use v_signal_nodes_runtime::realtime;
use v_signal_nodes_runtime::realtime::InputSource;

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

#[test]
fn resampling_doesnt_allocate() {
    let mut host = PluginHost::new();
    host.load(&StdPlugin).unwrap();
    let mut graph = Graph::new();
    let source = graph.add(host.build_node(GENERATOR).unwrap());
    let sink = graph.add(host.build_node(DEVICE_OUTPUT).unwrap());
    graph
        .connect(OutputRef::new(source, 0), InputRef::new(sink, 0))
        .unwrap();

    // Down and up.
    for (rate, device_rate) in [(192_000.0, 48_000.0), (44_100.0, 48_000.0)] {
        let (mut controller, renderer) = realtime::channel(rate, 64);
        controller
            .run_plan(compile(&graph).unwrap(), |step| {
                host.create_processor(&step.spec.type_id)
            })
            .unwrap();
        let mut resampled = Resampled::new(renderer, device_rate, 2).unwrap();
        let mut device = vec![0.0; 2 * 1000];

        let calls = allocator_calls(|| {
            for _ in 0..10 {
                resampled.render(&mut device);
            }
        });

        assert_eq!(calls, 0, "{rate} Hz to {device_rate} Hz");
        assert!(device.iter().any(|&sample| sample != 0.0));
    }
}

#[test]
fn taking_device_input_doesnt_allocate() {
    // Same rate, and resampled up.
    for device_rate in [48_000.0, 44_100.0] {
        let (mut device, ring) = rtrb::RingBuffer::new(48_000);
        let mut input = DeviceInput::new(ring, 2, device_rate, 48_000.0).unwrap();
        let mut samples = vec![0.0; 2 * 1000];
        let calls = allocator_calls(|| {
            for _ in 0..10 {
                // Some of it there, some missing.
                for _ in 0..1500 {
                    let _ = device.push(0.5);
                }
                input.read(&mut samples);
            }
        });
        assert_eq!(calls, 0, "from {device_rate} Hz");
    }
}

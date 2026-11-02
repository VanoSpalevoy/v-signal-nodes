//! What processors publish for their plot windows, on its way to whoever shows it.
//!
//! [`with_plots`] wraps a processor so that its [`ProcessIo::plot`] calls reach a
//! [`PlotReader`] per window of its spec. The wrapper goes wherever the processor
//! goes (into an engine, on to the next one with a new plan), so the engine knows
//! nothing about plots. A processor not wrapped publishes into nothing.
//!
//! The way across is a pair of lock-free queues per window, samples and frame headers,
//! with room for a few frames. Publishing allocates nothing and never
//! waits: a frame without room is lost, and readers only want the latest one anyway.

use std::ffi::c_void;
use std::slice;

use rtrb::{Consumer, Producer, RingBuffer};
use v_signal_nodes_plugin_api::{
    MAX_PLOT_POINTS, NodeProcessor, PluginError, ProcessIo, RawPlotSink,
};

/// Frames that can wait in a window's queues.
const QUEUED_FRAMES: usize = 4;

/// One frame of a plot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlotFrame {
    /// Distance between two samples on the horizontal axis.
    pub x_step: f32,
    pub samples: Vec<f32>,
}

/// The size of a frame in the samples queue.
#[derive(Clone, Copy)]
struct Header {
    x_step: f32,
    len: usize,
}

/// The publishing end for one window. A [`RawPlotSink`] points to it.
struct PlotWriter {
    headers: Producer<Header>,
    samples: Producer<f32>,
}

impl PlotWriter {
    fn publish(&mut self, x_step: f32, frame: &[f32]) {
        if self.headers.is_full() {
            return;
        }
        let Ok(chunk) = self.samples.write_chunk_uninit(frame.len()) else {
            return;
        };
        chunk.fill_from_iter(frame.iter().copied());
        // There's room: the only producer checked above.
        let _ = self.headers.push(Header {
            x_step,
            len: frame.len(),
        });
    }
}

/// [`RawPlotSink::publish`] of a [`PlotWriter`].
unsafe extern "C" fn publish(ctx: *mut c_void, x_step: f32, samples: *const f32, len: usize) {
    if len == 0 {
        return;
    }
    // SAFETY: `ctx` is a writer of the `Observed` that made the sink, which is the one
    // running now, and `ProcessIo::plot` passes a valid slice.
    unsafe {
        let writer = &mut *ctx.cast::<PlotWriter>();
        writer.publish(x_step, slice::from_raw_parts(samples, len));
    }
}

/// The reading end for one window.
pub struct PlotReader {
    headers: Consumer<Header>,
    samples: Consumer<f32>,
    latest: Option<PlotFrame>,
}

impl PlotReader {
    /// Takes in what was published since the last call, keeping the newest frame.
    pub fn poll(&mut self) {
        while let Ok(header) = self.headers.pop() {
            let Ok(chunk) = self.samples.read_chunk(header.len) else {
                // The writer pushes the samples before the header, so this can't be.
                return;
            };
            let frame = self.latest.get_or_insert_default();
            frame.x_step = header.x_step;
            frame.samples.clear();
            let (first, second) = chunk.as_slices();
            frame.samples.extend_from_slice(first);
            frame.samples.extend_from_slice(second);
            chunk.commit_all();
        }
    }

    /// The newest frame [`poll`](Self::poll) took in.
    pub fn latest(&self) -> Option<&PlotFrame> {
        self.latest.as_ref()
    }

    /// Whether the processor still exists, so more frames may come.
    pub fn is_live(&self) -> bool {
        !self.headers.is_abandoned()
    }
}

/// A processor with somewhere to publish its plots.
struct Observed {
    inner: Box<dyn NodeProcessor>,
    /// One per window. `sinks` point at them, so this never changes after
    /// [`with_plots`]: its heap buffer stays where it is when `Observed` moves.
    _writers: Vec<PlotWriter>,
    sinks: Vec<RawPlotSink>,
}

// SAFETY: the only non-`Send` fields are the sinks' pointers, to the writers owned by
// this same value, which is used by one thread at a time. A writer is `Send`.
unsafe impl Send for Observed {}

impl NodeProcessor for Observed {
    fn prepare(&mut self, sample_rate: f32, max_frames: usize) -> Result<(), PluginError> {
        self.inner.prepare(sample_rate, max_frames)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        let spec = io.spec();
        let position = io.position();
        let (frames, params, inputs, outputs) = io.raw_parts();
        // SAFETY: the parts come from a `ProcessIo`, which guarantees what `from_raw`
        // needs, and `io` isn't used while this one lives. There is one sink per
        // window (`with_plots` was given their number by the spec), each pointing to
        // a writer of this processor.
        let mut io = unsafe {
            ProcessIo::from_raw(frames, spec, params, inputs, outputs)
                .with_plots(&self.sinks)
                .with_position(position)
        };
        self.inner.process(&mut io)
    }
}

/// `processor` publishing its plots, and one reader per window, for a node whose spec
/// has `windows` windows.
pub fn with_plots(
    processor: Box<dyn NodeProcessor>,
    windows: usize,
) -> (Box<dyn NodeProcessor>, Vec<PlotReader>) {
    let mut writers = Vec::with_capacity(windows);
    let mut readers = Vec::with_capacity(windows);
    for _ in 0..windows {
        let (header_writer, header_reader) = RingBuffer::new(QUEUED_FRAMES);
        let (sample_writer, sample_reader) = RingBuffer::new(QUEUED_FRAMES * MAX_PLOT_POINTS);
        writers.push(PlotWriter {
            headers: header_writer,
            samples: sample_writer,
        });
        readers.push(PlotReader {
            headers: header_reader,
            samples: sample_reader,
            latest: None,
        });
    }
    let sinks = writers
        .iter_mut()
        .map(|writer| RawPlotSink {
            ctx: std::ptr::from_mut(writer).cast(),
            publish,
        })
        .collect();
    let observed = Observed {
        inner: processor,
        _writers: writers,
        sinks,
    };
    (Box::new(observed), readers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_plugin_api::{AxisUnit, NodeKind, NodeSocket, NodeSpec, PlotWindow};

    const SCREEN: PlotWindow = PlotWindow("screen");

    /// Publishes its input as a frame, doubling `x_step` with every call.
    struct Publisher(f32);

    impl NodeProcessor for Publisher {
        fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
            self.0 *= 2.0;
            io.plot(SCREEN, self.0, io.input(0))
        }
    }

    fn spec() -> NodeSpec {
        NodeSpec::new("test.scope", "Scope", NodeKind::Instrument)
            .input(NodeSocket::signal_in("in"))
            .window(SCREEN.spec("Screen", AxisUnit::Seconds, AxisUnit::Volts))
    }

    fn process(processor: &mut dyn NodeProcessor, spec: &NodeSpec, input: &[f32]) {
        let inputs = [v_signal_nodes_plugin_api::RawBuffer::input(input)];
        // SAFETY: a mono input of `input.len()` frames, as the spec wants, no outputs.
        let mut io = unsafe { ProcessIo::from_raw(input.len(), spec, &[], &inputs, &[]) };
        processor.process(&mut io).unwrap();
    }

    #[test]
    fn the_reader_gets_the_newest_frame() {
        let spec = spec();
        let (mut processor, mut readers) = with_plots(Box::new(Publisher(1.0)), 1);
        let reader = &mut readers[0];
        reader.poll();
        assert_eq!(reader.latest(), None);

        process(processor.as_mut(), &spec, &[1.0, 2.0]);
        process(processor.as_mut(), &spec, &[3.0]);
        reader.poll();

        let frame = reader.latest().unwrap();
        assert_eq!(frame.x_step, 4.0);
        assert_eq!(frame.samples, [3.0]);
    }

    #[test]
    fn frames_without_room_are_lost_and_the_next_ones_arrive() {
        let spec = spec();
        let (mut processor, mut readers) = with_plots(Box::new(Publisher(1.0)), 1);
        let reader = &mut readers[0];

        for _ in 0..QUEUED_FRAMES + 3 {
            process(processor.as_mut(), &spec, &[1.0]);
        }
        reader.poll();
        let last_queued = 2.0_f32.powi(QUEUED_FRAMES as i32);
        assert_eq!(reader.latest().unwrap().x_step, last_queued);

        process(processor.as_mut(), &spec, &[1.0, 2.0]);
        reader.poll();
        assert_eq!(reader.latest().unwrap().samples, [1.0, 2.0]);
    }

    #[test]
    fn the_last_frame_stays_after_the_processor_is_gone() {
        let spec = spec();
        let (mut processor, mut readers) = with_plots(Box::new(Publisher(1.0)), 1);
        process(processor.as_mut(), &spec, &[5.0]);
        assert!(readers[0].is_live());
        drop(processor);
        assert!(!readers[0].is_live());

        readers[0].poll();

        assert_eq!(readers[0].latest().unwrap().samples, [5.0]);
    }
}

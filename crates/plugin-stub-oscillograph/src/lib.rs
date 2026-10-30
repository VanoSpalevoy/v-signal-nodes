//! A placeholder oscillograph (`stub-oscillograph.probe`): no graphics, just some
//! data out of a running graph, until plugins have a way to show anything.
//!
//! The node passes its input through unchanged and prints the minimum, maximum and
//! RMS of every half second of signal to stdout:
//!
//! ```text
//! [oscillograph 1] t=1.500s min=-0.998 max=0.998 rms=0.707
//! ```
//!
//! It's a processor, not an output node: output nodes are mixed to the device, and
//! looking at a signal shouldn't make it heard. So, like any processor, it runs only
//! when its output leads to an output node. The number in the line counts processors
//! since the start of the program; it isn't the node's number on the canvas.
//!
//! `process` may run on a real-time thread, where printing isn't allowed: it only
//! pushes finished reports into a lock-free queue, and a thread started in `prepare`
//! prints them. That thread ends by itself once the processor is gone.

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};
use v_signal_nodes_plugin_api::{
    NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, Plugin, PluginError, ProcessIo,
    Registrar, Rgb, TextKey,
};

/// Type id of the oscillograph node.
pub const OSCILLOGRAPH: &str = "stub-oscillograph.probe";

/// Seconds of signal one report covers.
const REPORT_SECONDS: f32 = 0.5;
/// Reports waiting to be printed, at most. A full queue drops new ones. A one-pass
/// run makes them much faster than real time, but only a few dozen in all.
const REPORT_CAPACITY: usize = 64;
/// How often the printing thread looks for new reports.
const PRINT_INTERVAL: Duration = Duration::from_millis(50);

/// The number of the next processor, to tell their lines apart.
static NEXT_INSTANCE: AtomicUsize = AtomicUsize::new(1);

/// The stub oscillograph plugin, named `"stub-oscillograph"`.
pub struct StubOscillographPlugin;

impl Plugin for StubOscillographPlugin {
    fn name(&self) -> &str {
        "stub-oscillograph"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(&self, registrar: &mut dyn Registrar) {
        registrar.register_node(Box::new(Oscillograph));
    }
}

/// [`OSCILLOGRAPH`]: one mono input, passed through to one mono output. Gray, its
/// own color, so it doesn't pass for a real processor.
struct Oscillograph;

impl NodeProvider for Oscillograph {
    fn spec(&self) -> NodeSpec {
        NodeSpec::new(OSCILLOGRAPH, "Oscillograph", NodeKind::Processor)
            .color(Rgb::new(160, 164, 172))
            .input(NodeSocket::signal_in("In"))
            .output(NodeSocket::signal_out("Out"))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Осциллограф"),
                    (TextKey::Input(0), "Вход"),
                    (TextKey::Output(0), "Выход"),
                ],
            )
    }

    fn create_processor(&self) -> Box<dyn NodeProcessor> {
        Box::new(OscillographProcessor::new(
            NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
        ))
    }
}

/// What the signal did over one stretch of [`REPORT_SECONDS`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Report {
    /// The frame the stretch starts at, counted from the last reset.
    start: u64,
    min: f32,
    max: f32,
    rms: f32,
}

/// The stretch of signal being measured.
#[derive(Clone, Copy)]
struct Window {
    frames: usize,
    min: f32,
    max: f32,
    squares: f64,
}

impl Window {
    const EMPTY: Self = Self {
        frames: 0,
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
        squares: 0.0,
    };

    fn add(&mut self, sample: f32) {
        self.frames += 1;
        self.min = self.min.min(sample);
        self.max = self.max.max(sample);
        self.squares += f64::from(sample) * f64::from(sample);
    }

    fn report(&self, start: u64) -> Report {
        Report {
            start,
            min: self.min,
            max: self.max,
            rms: (self.squares / self.frames as f64).sqrt() as f32,
        }
    }
}

/// Measures the signal and hands the reports over to the printing thread.
struct OscillographProcessor {
    instance: usize,
    /// Frames per report, set by `prepare`.
    window_frames: usize,
    /// Frames in reports already made, since the last reset.
    reported: u64,
    window: Window,
    /// The way to the printing thread, set by `prepare`.
    reports: Option<Producer<Report>>,
}

impl OscillographProcessor {
    fn new(instance: usize) -> Self {
        Self {
            instance,
            window_frames: 0,
            reported: 0,
            window: Window::EMPTY,
            reports: None,
        }
    }
}

impl NodeProcessor for OscillographProcessor {
    fn prepare(&mut self, sample_rate: f32, _max_frames: usize) -> Result<(), PluginError> {
        self.window_frames = ((sample_rate * REPORT_SECONDS) as usize).max(1);
        self.reset();
        let (producer, consumer) = RingBuffer::new(REPORT_CAPACITY);
        let instance = self.instance;
        thread::Builder::new()
            .name(format!("oscillograph-{instance}"))
            .spawn(move || print_reports(consumer, instance, sample_rate, io::stdout()))
            .map_err(|err| PluginError::new(format!("can't start printing: {err}")))?;
        // Replacing an earlier producer ends the earlier thread.
        self.reports = Some(producer);
        Ok(())
    }

    fn reset(&mut self) {
        self.reported = 0;
        self.window = Window::EMPTY;
    }

    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
        let input = io.input(0);
        io.output(0).copy_from_slice(input);
        let Some(reports) = &mut self.reports else {
            return Ok(());
        };
        for &sample in input {
            self.window.add(sample);
            if self.window.frames == self.window_frames {
                // A full queue only loses this report.
                let _ = reports.push(self.window.report(self.reported));
                self.reported += self.window_frames as u64;
                self.window = Window::EMPTY;
            }
        }
        Ok(())
    }
}

/// Writes reports to `out` as they come, until the processor is gone and all it sent
/// is written, or `out` stops taking them (a closed stdout).
fn print_reports(
    mut reports: Consumer<Report>,
    instance: usize,
    sample_rate: f32,
    mut out: impl Write,
) {
    loop {
        // Checked before emptying the queue, so nothing pushed last is missed.
        let finished = reports.is_abandoned();
        while let Ok(report) = reports.pop() {
            let seconds = report.start as f64 / f64::from(sample_rate);
            let written = writeln!(
                out,
                "[oscillograph {instance}] t={seconds:.3}s min={:.3} max={:.3} rms={:.3}",
                report.min, report.max, report.rms
            );
            if written.is_err() {
                return;
            }
        }
        if finished {
            return;
        }
        thread::sleep(PRINT_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_plugin_api::RawBuffer;

    /// A processor reporting every `window_frames` into the returned queue, with no
    /// printing thread, and the node's spec.
    fn processor(window_frames: usize) -> (OscillographProcessor, Consumer<Report>, NodeSpec) {
        let (producer, consumer) = RingBuffer::new(REPORT_CAPACITY);
        let mut processor = OscillographProcessor::new(1);
        processor.window_frames = window_frames;
        processor.reports = Some(producer);
        (processor, consumer, Oscillograph.spec())
    }

    /// Runs one block through `processor` and returns its output.
    fn process(processor: &mut OscillographProcessor, spec: &NodeSpec, input: &[f32]) -> Vec<f32> {
        let mut output = vec![0.0; input.len()];
        let inputs = [RawBuffer::input(input)];
        let outputs = [RawBuffer::output(&mut output)];
        // SAFETY: mono buffers of `input.len()` frames, as the spec's sockets want,
        // live for the call; the output overlaps nothing.
        let mut io = unsafe { ProcessIo::from_raw(input.len(), spec, &[], &inputs, &outputs) };
        processor.process(&mut io).unwrap();
        output
    }

    #[test]
    fn the_spec_is_valid() {
        assert!(Oscillograph.spec().validate().is_ok());
    }

    #[test]
    fn prepare_sets_the_report_length_from_the_sample_rate() {
        let mut processor = OscillographProcessor::new(1);

        processor.prepare(48_000.0, 512).unwrap();
        assert_eq!(processor.window_frames, 24_000);
        assert!(processor.reports.is_some());

        processor.prepare(0.5, 512).unwrap();
        assert_eq!(processor.window_frames, 1, "never an empty report");
    }

    #[test]
    fn passes_the_signal_through_and_reports_across_blocks() {
        let (mut processor, mut reports, spec) = processor(4);

        let block = [1.0, -1.0, 1.0, -1.0, 0.5, 0.5];
        assert_eq!(process(&mut processor, &spec, &block), block);
        assert_eq!(
            reports.pop(),
            Ok(Report {
                start: 0,
                min: -1.0,
                max: 1.0,
                rms: 1.0
            })
        );
        assert!(reports.pop().is_err(), "the second report isn't full yet");

        process(&mut processor, &spec, &[0.5, 0.5]);
        assert_eq!(
            reports.pop(),
            Ok(Report {
                start: 4,
                min: 0.5,
                max: 0.5,
                rms: 0.5
            })
        );
    }

    #[test]
    fn one_block_can_make_several_reports() {
        let (mut processor, reports, spec) = processor(2);

        process(&mut processor, &spec, &[0.0; 6]);

        assert_eq!(reports.slots(), 3);
    }

    #[test]
    fn a_full_queue_only_loses_reports() {
        let (mut processor, mut reports, spec) = processor(1);

        process(&mut processor, &spec, &[0.25; REPORT_CAPACITY + 10]);

        assert_eq!(reports.slots(), REPORT_CAPACITY);
        assert_eq!(reports.pop().unwrap().start, 0);
    }

    #[test]
    fn reset_starts_over() {
        let (mut processor, mut reports, spec) = processor(2);
        process(&mut processor, &spec, &[1.0, 1.0, 1.0]);
        reports.pop().unwrap();

        processor.reset();
        process(&mut processor, &spec, &[0.5, 0.5]);

        let report = reports.pop().unwrap();
        assert_eq!(report.start, 0);
        assert_eq!(report.max, 0.5, "the sample before the reset is forgotten");
    }

    #[test]
    fn prints_everything_sent_and_ends_when_the_processor_is_gone() {
        let (mut processor, reports, spec) = processor(2);
        process(&mut processor, &spec, &[0.5, -0.5, 0.25, 0.25]);
        drop(processor);
        let mut out = Vec::new();

        print_reports(reports, 7, 2.0, &mut out);

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "[oscillograph 7] t=0.000s min=-0.500 max=0.500 rms=0.500\n\
             [oscillograph 7] t=1.000s min=0.250 max=0.250 rms=0.250\n"
        );
    }
}

//! Running the edited graph: the editor's side of the thread model.
//!
//! The UI thread owns the graph, so it compiles it (once per
//! [`Graph::revision`](v_signal_nodes_core::Graph::revision), also while nothing
//! runs, so the debug panel can show compile errors up front), creates processors
//! through the plugin host and hands them over ready to run. The other threads only
//! ever get a plan and processors, never the graph:
//!
//! - **Real time** ([`Command::RunRealtime`](super::commands::Command)): the device's
//!   thread runs a `runtime::realtime::Renderer`. While it runs, every change to the
//!   graph becomes a new plan, sent through a `Controller`; nodes that stay keep
//!   their processors. A graph that stops compiling silences the output until it
//!   compiles again.
//! - **One pass** ([`Command::RunAsync`](super::commands::Command)): a background
//!   thread runs the plan once over [`ONE_PASS_FRAMES`] in big blocks, branches in
//!   parallel, and sends back a summary. There is no file or display output yet, so
//!   the summary is what comes of it.
//!
//! Both are polled once per frame by [`update`](VSignalNodesApp::update_execution).

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use v_signal_nodes_core::{CompileError, Plan, compile};
use v_signal_nodes_device::{DeviceError, Output, OutputStream};
use v_signal_nodes_runtime::realtime::{self, Controller};
use v_signal_nodes_runtime::{Engine, RuntimeError, processors_for};

use super::VSignalNodesApp;

/// Longest block of real-time processing, in frames: device blocks are cut to this.
const REALTIME_MAX_FRAMES: usize = 512;
/// Block size of one-pass processing: memory is this × the number of live buffers.
const ONE_PASS_MAX_FRAMES: usize = 1 << 16;
/// Length of a one-pass run. There is no way to choose it in the UI yet.
const ONE_PASS_FRAMES: usize = 1 << 20;
/// Sample rate of a one-pass run: no device sets it, and the UI can't yet.
const ONE_PASS_SAMPLE_RATE: f32 = 48_000.0;
/// How often the UI wakes up to poll what's running.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A real-time run in progress.
struct RealtimeRun {
    /// Dropped first, which stops the device before the controller goes away.
    _stream: OutputStream,
    controller: Controller,
    device_errors: Receiver<DeviceError>,
}

/// What came of a one-pass run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OnePassSummary {
    pub frames: usize,
    pub elapsed: Duration,
    /// Largest absolute sample the output nodes got.
    pub peak: f32,
}

/// The state of running the graph. Derived from nothing saved; never in a document.
pub(super) struct Execution {
    /// The graph revision `compile_error` belongs to.
    compiled_revision: u64,
    /// Why the graph at `compiled_revision` doesn't compile, if it doesn't.
    compile_error: Option<CompileError>,
    /// The running plan is older than the graph (a send was refused as busy, or the
    /// graph changed): send a new one.
    plan_outdated: bool,
    realtime: Option<RealtimeRun>,
    one_pass: Option<Receiver<Result<OnePassSummary, RuntimeError>>>,
    /// The last finished one-pass run.
    one_pass_result: Option<OnePassSummary>,
    /// What went wrong with the last run, in the system's or the plugin's words.
    failure: Option<String>,
}

impl Default for Execution {
    /// The state for an empty graph, which has no output node.
    fn default() -> Self {
        Self {
            compiled_revision: 0,
            compile_error: Some(CompileError::NoOutput),
            plan_outdated: false,
            realtime: None,
            one_pass: None,
            one_pass_result: None,
            failure: None,
        }
    }
}

impl Execution {
    /// Why the graph can't run, as of the last compile.
    pub(super) fn compile_error(&self) -> Option<CompileError> {
        self.compile_error
    }

    pub(super) fn is_running_realtime(&self) -> bool {
        self.realtime.is_some()
    }

    pub(super) fn is_running_one_pass(&self) -> bool {
        self.one_pass.is_some()
    }

    pub(super) fn one_pass_result(&self) -> Option<OnePassSummary> {
        self.one_pass_result
    }

    pub(super) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
}

impl VSignalNodesApp {
    /// A plan of the graph as it is now.
    fn compile_graph(&self) -> Result<Plan, CompileError> {
        compile(&self.graph)
    }

    /// Brings the compile status up to date if the graph changed since.
    pub(super) fn refresh_compile_status(&mut self) {
        if self.execution.compiled_revision != self.graph.revision() {
            self.mark_graph_replaced();
        }
    }

    /// Recompiles for the status, whatever the revision says: for after `graph` was
    /// replaced by another one, whose revision numbers mean nothing here.
    pub(super) fn mark_graph_replaced(&mut self) {
        self.execution.compiled_revision = self.graph.revision();
        self.execution.compile_error = self.compile_graph().err();
        self.execution.plan_outdated = true;
    }

    /// Per-frame work: the compile status, a new plan for a running device, and what
    /// the running threads sent back. Keeps the UI waking up while anything runs.
    pub(super) fn update_execution(&mut self, ctx: &egui::Context) {
        self.refresh_compile_status();
        self.send_outdated_plan();
        self.poll_realtime();
        self.poll_one_pass();
        if self.execution.realtime.is_some() || self.execution.one_pass.is_some() {
            ctx.request_repaint_after(POLL_INTERVAL);
        }
    }

    /// Starts real-time processing on the default output device, or stops it.
    pub(super) fn toggle_realtime(&mut self) {
        if self.execution.realtime.take().is_some() {
            self.execution.failure = None;
            return;
        }
        self.execution.failure = match self.start_realtime() {
            Ok(run) => {
                self.execution.realtime = Some(run);
                None
            }
            Err(failure) => Some(failure),
        };
    }

    /// Opens the device and runs the graph on it. The device starts silent and gets
    /// the plan through the same path later changes take.
    fn start_realtime(&mut self) -> Result<RealtimeRun, String> {
        let plan = self.compile_graph().map_err(|err| err.to_string())?;
        let output = Output::default_device().map_err(|err| err.to_string())?;
        let (mut controller, renderer) =
            realtime::channel(output.sample_rate(), REALTIME_MAX_FRAMES);
        let plugins = &self.plugins;
        controller
            .run_plan(plan, |step| plugins.create_processor(&step.spec.type_id))
            .map_err(|err| err.to_string())?;
        let (errors, device_errors) = mpsc::channel();
        let stream = output
            .start(renderer, errors)
            .map_err(|err| err.to_string())?;
        self.execution.plan_outdated = false;
        Ok(RealtimeRun {
            _stream: stream,
            controller,
            device_errors,
        })
    }

    /// Sends the graph's plan to the running device if it's newer than what runs
    /// there. A refused send (the device thread is behind) is tried again next frame.
    fn send_outdated_plan(&mut self) {
        let plugins = &self.plugins;
        let execution = &mut self.execution;
        if !execution.plan_outdated {
            return;
        }
        let Some(run) = &mut execution.realtime else {
            execution.plan_outdated = false;
            return;
        };
        let result = match compile(&self.graph) {
            Ok(plan) => run
                .controller
                .run_plan(plan, |step| plugins.create_processor(&step.spec.type_id)),
            // Nothing valid to run: silence, until the graph compiles again. The
            // reason is already shown as the compile error.
            Err(_) => run.controller.clear(),
        };
        match result {
            Ok(()) => {
                execution.plan_outdated = false;
                execution.failure = None;
            }
            Err(RuntimeError::Busy) => {}
            Err(err) => {
                // Not retried: the same graph would fail the same way. The old plan
                // keeps running until the next change.
                execution.plan_outdated = false;
                execution.failure = Some(err.to_string());
            }
        }
    }

    /// Takes in what the device's thread sent back. A device error ends the run; a
    /// failed plan leaves it silent, waiting for the next change.
    fn poll_realtime(&mut self) {
        let Some(run) = &mut self.execution.realtime else {
            return;
        };
        if let Some(error) = run.controller.poll() {
            self.execution.failure = Some(error.to_string());
        }
        if let Ok(error) = run.device_errors.try_recv() {
            self.execution.failure = Some(error.to_string());
            self.execution.realtime = None;
        }
    }

    /// Starts the one-pass run on a background thread. The processors are created
    /// here, on the thread owning the plugin host, and move over ready to run.
    pub(super) fn start_one_pass(&mut self) {
        match self.build_one_pass_engine() {
            Ok(engine) => {
                let (result, receiver) = mpsc::channel();
                thread::spawn(move || {
                    // The receiver is gone only when the editor is closing.
                    let _ = result.send(run_one_pass(engine));
                });
                self.execution.one_pass = Some(receiver);
                self.execution.one_pass_result = None;
                self.execution.failure = None;
            }
            Err(failure) => self.execution.failure = Some(failure),
        }
    }

    fn build_one_pass_engine(&self) -> Result<Engine, String> {
        let plan = self.compile_graph().map_err(|err| err.to_string())?;
        let processors = processors_for(&plan, &[], |step| {
            self.plugins.create_processor(&step.spec.type_id)
        })
        .map_err(|err| err.to_string())?;
        Engine::new(plan, processors, ONE_PASS_SAMPLE_RATE, ONE_PASS_MAX_FRAMES)
            .map_err(|err| err.to_string())
    }

    fn poll_one_pass(&mut self) {
        let Some(receiver) = &self.execution.one_pass else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.execution.one_pass = None;
                self.execution.failure = Some("the one-pass thread ended early".to_owned());
                return;
            }
        };
        self.execution.one_pass = None;
        match result {
            Ok(summary) => self.execution.one_pass_result = Some(summary),
            Err(error) => self.execution.failure = Some(error.to_string()),
        }
    }
}

/// Runs `engine` once over [`ONE_PASS_FRAMES`], on as many threads as the machine
/// offers, and measures what the output nodes got.
fn run_one_pass(mut engine: Engine) -> Result<OnePassSummary, RuntimeError> {
    let threads = thread::available_parallelism().map_or(1, usize::from);
    let start = Instant::now();
    let mut peak = 0.0_f32;
    engine.run_parallel(ONE_PASS_FRAMES, threads, |engine, frames| {
        for output in engine.plan().outputs() {
            for sample in engine.delivered(output, frames) {
                peak = peak.max(sample.abs());
            }
        }
    })?;
    Ok(OnePassSummary {
        frames: ONE_PASS_FRAMES,
        elapsed: start.elapsed(),
        peak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::commands::Command;
    use v_signal_nodes_core::{InputRef, OutputRef};
    use v_signal_nodes_plugin_std::{GAIN, SINK, SOURCE};

    /// An app with source → gain → sink.
    fn runnable() -> VSignalNodesApp {
        let mut app = VSignalNodesApp::default();
        let add = |app: &mut VSignalNodesApp, type_id, x| {
            app.add_node_at(app.kind_of(type_id).unwrap(), egui::pos2(x, 0.0))
                .unwrap()
        };
        let source = add(&mut app, SOURCE, 0.0);
        let gain = add(&mut app, GAIN, 300.0);
        let sink = add(&mut app, SINK, 600.0);
        assert!(app.connect(OutputRef::new(source, 0), InputRef::new(gain, 0)));
        assert!(app.connect(OutputRef::new(gain, 0), InputRef::new(sink, 0)));
        app
    }

    #[test]
    fn the_compile_status_follows_the_graph() {
        let mut app = VSignalNodesApp::default();
        assert_eq!(
            app.execution.compile_error(),
            Some(CompileError::NoOutput),
            "an empty graph"
        );

        let sink = app
            .add_node_at(app.kind_of(SINK).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        app.refresh_compile_status();
        assert_eq!(app.execution.compile_error(), None);

        app.remove_node(sink);
        app.refresh_compile_status();
        assert_eq!(app.execution.compile_error(), Some(CompileError::NoOutput));
    }

    #[test]
    fn opening_a_document_refreshes_the_status() {
        let document = runnable().to_document();
        let mut app = VSignalNodesApp::default();

        app.open_document(&document).unwrap();

        assert_eq!(app.execution.compile_error(), None);
    }

    #[test]
    fn commands_need_a_graph_that_compiles() {
        let app = VSignalNodesApp::default();
        assert!(!app.can_execute(Command::RunRealtime));
        assert!(!app.can_execute(Command::RunAsync));

        let mut app = runnable();
        app.refresh_compile_status();
        assert!(app.can_execute(Command::RunRealtime));
        assert!(app.can_execute(Command::RunAsync));
    }

    /// The whole real-time path on the real device: start, a changed graph while
    /// running (a new plan), a graph that stops compiling, stop. Only unconnected
    /// output nodes, so it's silent. Ignored because CI machines have no device.
    #[test]
    #[ignore = "needs an output device"]
    fn runs_on_the_device_and_follows_the_graph() {
        let mut app = VSignalNodesApp::default();
        let ctx = egui::Context::default();
        let sink = app
            .add_node_at(app.kind_of(SINK).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let settle = |app: &mut VSignalNodesApp| {
            for _ in 0..10 {
                app.update_execution(&ctx);
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(app.execution.failure(), None);
            assert!(app.execution.is_running_realtime());
        };

        app.execute(Command::RunRealtime);
        settle(&mut app);

        let second = app
            .add_node_at(app.kind_of(SINK).unwrap(), egui::pos2(300.0, 0.0))
            .unwrap();
        settle(&mut app);
        assert!(!app.execution.plan_outdated);

        app.remove_node(sink);
        app.remove_node(second);
        settle(&mut app);
        assert_eq!(app.execution.compile_error(), Some(CompileError::NoOutput));
        assert!(
            app.can_execute(Command::RunRealtime),
            "stopping is always possible"
        );

        app.execute(Command::RunRealtime);
        assert!(!app.execution.is_running_realtime());
    }

    #[test]
    fn one_pass_runs_on_a_thread_and_reports() {
        let mut app = runnable();
        let ctx = egui::Context::default();

        app.execute(Command::RunAsync);
        assert!(app.execution.is_running_one_pass());
        assert!(!app.can_execute(Command::RunAsync), "one at a time");

        let deadline = Instant::now() + Duration::from_secs(60);
        while app.execution.is_running_one_pass() {
            assert!(Instant::now() < deadline, "the run doesn't finish");
            app.update_execution(&ctx);
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(app.execution.failure(), None);
        let summary = app.execution.one_pass_result().unwrap();
        assert_eq!(summary.frames, ONE_PASS_FRAMES);
        assert!(summary.peak > 0.9, "a full-scale source at gain 1");
    }
}

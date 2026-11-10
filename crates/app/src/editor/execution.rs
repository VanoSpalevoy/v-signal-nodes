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
//!   A changed parameter is no new plan: it goes straight to the running processors
//!   ([`send_param`](Execution::send_param)), so moving a control is cheap.
//! - **One pass** ([`Command::RunAsync`](super::commands::Command)): a background
//!   thread runs the plan once over [`ONE_PASS_FRAMES`] in big blocks, branches in
//!   parallel, and sends back a summary. There is no file output yet, so the summary
//!   and the plot windows are what comes of it.
//!
//! Both are polled once per frame by [`update`](VSignalNodesApp::update_execution).
//! A node that fails on the device leaves it silent; after [`RETRY_DELAY`] the plan
//! goes again, with a new processor for that node.
//!
//! Opening another graph ([`replace_graph`](VSignalNodesApp::replace_graph)) starts a
//! real-time run over: node ids of different graphs may coincide, so processors and
//! plots kept by node id can't carry over.
//!
//! Both run at the project's sample rate (`VSignalNodesApp::sample_rate`), whatever
//! the device's: the device driver resamples. Processors are prepared for one rate, so
//! a new rate starts a real-time run over with new ones
//! ([`set_sample_rate`](VSignalNodesApp::set_sample_rate)).
//!
//! Nodes with plot windows get their processor wrapped when it's created
//! ([`make_processor`]). It publishes from whichever thread runs it, and a window
//! reads the newest frame from the node's [`PlotFeed`] ([`Execution::plot_feed`]) on
//! its own schedule; the last frame stays on show after a run ends.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use v_signal_nodes_core::{
    CompileError, Error, Graph, NodeId, ParamValue, Plan, Step, Unit, compile,
};
use v_signal_nodes_device::{DeviceError, Output, OutputStream};
use v_signal_nodes_plugin_host::PluginHost;
#[cfg(test)]
use v_signal_nodes_runtime::plots::PlotFrame;
use v_signal_nodes_runtime::plots::{self, PlotReader};
use v_signal_nodes_runtime::realtime::{self, Controller};
use v_signal_nodes_runtime::{Engine, NodeProcessor, RuntimeError, processors_for};

use super::VSignalNodesApp;
use crate::document::SAMPLE_RATE_RANGE;

/// Longest block of real-time processing, in frames: device blocks are cut to this.
const REALTIME_MAX_FRAMES: usize = 512;
/// Block size of one-pass processing: memory is this × the number of live buffers.
const ONE_PASS_MAX_FRAMES: usize = 1 << 16;
/// Length of a one-pass run, in frames at the project's rate. There is no way to
/// choose it in the UI yet.
const ONE_PASS_FRAMES: usize = 1 << 20;
/// Fewest samples per period a frequency may have before the editor warns about it:
/// with fewer, a sine looks like a triangle on screen and the harmonics of other
/// waveforms fold back below half the sample rate.
pub(super) const MIN_SAMPLES_PER_PERIOD: f32 = 4.0;
/// How long after a node failed on the device it gets a new processor: soon enough to
/// come back by itself, seldom enough that a node failing every time doesn't rebuild
/// every frame.
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// How often the UI wakes up to poll what's running.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Where the plots of one window of a node arrive. Shared, because that window repaints
/// on its own, also while the main window doesn't.
pub(super) type PlotFeed = Arc<Mutex<PlotReader>>;

/// The reader of `feed`. Nothing holding it can leave it half-changed, so a panic
/// elsewhere while it was held doesn't matter.
pub(super) fn read_feed(feed: &PlotFeed) -> MutexGuard<'_, PlotReader> {
    feed.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A new processor for `step`, publishing to `plots` if its node has windows. The
/// feeds replace those of an earlier processor of the node.
fn make_processor(
    plugins: &PluginHost,
    plots: &mut HashMap<NodeId, Vec<PlotFeed>>,
    step: &Step,
) -> Result<Box<dyn NodeProcessor>, Error> {
    let processor = plugins.create_processor(&step.spec.type_id)?;
    let windows = step.spec.windows.len();
    if windows == 0 {
        return Ok(processor);
    }
    let (processor, readers) = plots::with_plots(processor, windows);
    let feeds = readers
        .into_iter()
        .map(|reader| Arc::new(Mutex::new(reader)))
        .collect();
    plots.insert(step.node, feeds);
    Ok(processor)
}

/// A real-time run in progress.
struct RealtimeRun {
    /// Dropped first, which stops the device before the controller goes away.
    _stream: OutputStream,
    controller: Controller,
    device_errors: Receiver<DeviceError>,
    /// When to send the plan again because a node failed, if one did.
    retry_at: Option<Instant>,
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
    /// Where the plots of nodes with windows arrive, one per window of the node.
    plots: HashMap<NodeId, Vec<PlotFeed>>,
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
            plots: HashMap::new(),
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

    /// Where the plots of window `window` of `node` arrive, once a run made one.
    pub(super) fn plot_feed(&self, node: NodeId, window: usize) -> Option<PlotFeed> {
        self.plots.get(&node)?.get(window).cloned()
    }

    /// The newest frame published to window `window` of `node`, if any.
    #[cfg(test)]
    fn plot_frame(&self, node: NodeId, window: usize) -> Option<PlotFrame> {
        read_feed(&self.plot_feed(node, window)?).latest().cloned()
    }

    /// Passes a changed parameter on to the running device, which takes it from its
    /// next block on. If its queue is full, a new plan carries the value instead.
    pub(super) fn send_param(&mut self, node: NodeId, index: usize, value: ParamValue) {
        if let Some(run) = &mut self.realtime
            && !run.controller.set_param(node, index, value)
        {
            self.plan_outdated = true;
        }
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
            self.recompile_status();
        }
    }

    /// Recompiles for the status, whatever the revision says, and marks the running
    /// plan outdated.
    fn recompile_status(&mut self) {
        self.execution.compiled_revision = self.graph.revision();
        self.execution.compile_error = self.compile_graph().err();
        self.execution.plan_outdated = true;
    }

    /// Puts `graph`, to run at `sample_rate` (within [`SAMPLE_RATE_RANGE`]), in place
    /// of the edited one. Another graph numbers its nodes anew, so its ids may be the
    /// old nodes' ids, and nothing keyed by node id may carry over: the plots go, and a
    /// real-time run starts over with new processors for every node, instead of
    /// handing the old nodes' processors to new nodes of the same id. Its revisions
    /// mean nothing here either, so the status is recompiled.
    pub(super) fn replace_graph(&mut self, graph: Graph, sample_rate: u32) {
        debug_assert!(SAMPLE_RATE_RANGE.contains(&sample_rate));
        self.graph = graph;
        self.sample_rate = sample_rate;
        self.execution.plots.clear();
        self.recompile_status();
        if self.execution.is_running_realtime() {
            self.stop_realtime();
            self.toggle_realtime();
        }
    }

    /// Per-frame work: the compile status, a new plan for a running device, and what
    /// the running threads sent back. Keeps the UI waking up while anything runs.
    pub(super) fn update_execution(&mut self, ctx: &egui::Context) {
        self.refresh_compile_status();
        self.retry_failed_nodes();
        self.send_outdated_plan();
        self.poll_realtime();
        self.poll_one_pass();
        self.poll_plots();
        if self.execution.realtime.is_some() || self.execution.one_pass.is_some() {
            ctx.request_repaint_after(POLL_INTERVAL);
        }
    }

    /// Takes in the newest plots, and forgets those of nodes that are gone.
    fn poll_plots(&mut self) {
        let graph = &self.graph;
        self.execution.plots.retain(|&node, feeds| {
            feeds.iter().for_each(|feed| read_feed(feed).poll());
            graph.contains(node)
        });
    }

    /// Starts real-time processing on the default output device, or stops it.
    pub(super) fn toggle_realtime(&mut self) {
        if self.execution.is_running_realtime() {
            self.stop_realtime();
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

    /// Stops real-time processing, if it runs.
    pub(super) fn stop_realtime(&mut self) {
        self.execution.realtime = None;
        self.execution.failure = None;
    }

    /// The samples per second the graph runs at.
    pub(super) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frequencies set too high for the sample rate: every parameter in
    /// [`Unit::Hertz`] above `sample_rate / MIN_SAMPLES_PER_PERIOD`, as
    /// `(node, parameter index, frequency)`, by node id. Checked on every node, also
    /// one that doesn't run yet, so the warning comes before the run.
    pub(super) fn few_samples_per_period(&self) -> impl Iterator<Item = (NodeId, usize, f32)> {
        let limit = self.sample_rate as f32 / MIN_SAMPLES_PER_PERIOD;
        self.graph.nodes().flat_map(move |(id, node)| {
            node.spec()
                .params
                .iter()
                .zip(node.params())
                .enumerate()
                .filter_map(move |(index, (spec, value))| {
                    let frequency = value.as_number()?;
                    (spec.unit == Some(Unit::Hertz) && frequency > limit)
                        .then_some((id, index, frequency))
                })
        })
    }

    /// Runs the graph at `rate` from now on: a real-time run starts over with new
    /// processors, prepared for it; a one-pass run keeps its rate until it ends.
    /// Refuses (returns `false`) a rate outside [`SAMPLE_RATE_RANGE`].
    pub(super) fn set_sample_rate(&mut self, rate: u32) -> bool {
        if !SAMPLE_RATE_RANGE.contains(&rate) {
            return false;
        }
        if rate != self.sample_rate {
            self.sample_rate = rate;
            if self.execution.is_running_realtime() {
                self.stop_realtime();
                self.toggle_realtime();
            }
        }
        true
    }

    /// Opens the device and runs the graph on it. The device starts silent and gets
    /// the plan through the same path later changes take.
    fn start_realtime(&mut self) -> Result<RealtimeRun, String> {
        let plan = self.compile_graph().map_err(|err| err.to_string())?;
        let output = Output::default_device().map_err(|err| err.to_string())?;
        let (mut controller, renderer) =
            realtime::channel(self.sample_rate as f32, REALTIME_MAX_FRAMES);
        let plugins = &self.plugins;
        let plots = &mut self.execution.plots;
        controller
            .run_plan(plan, |step| make_processor(plugins, plots, step))
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
            retry_at: None,
        })
    }

    /// Once [`RETRY_DELAY`] has passed since a node failed on the device, sends the
    /// plan again, even of an unchanged graph: the failed nodes get new processors in
    /// it. Without this, the output would stay silent until the next edit.
    fn retry_failed_nodes(&mut self) {
        if let Some(run) = &mut self.execution.realtime
            && run.retry_at.is_some_and(|at| at <= Instant::now())
        {
            run.retry_at = None;
            self.execution.plan_outdated = true;
        }
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
        let plots = &mut execution.plots;
        let result = match compile(&self.graph) {
            Ok(plan) => run
                .controller
                .run_plan(plan, |step| make_processor(plugins, plots, step)),
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
            if run.controller.has_failed_nodes() && run.retry_at.is_none() {
                run.retry_at = Some(Instant::now() + RETRY_DELAY);
            }
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

    fn build_one_pass_engine(&mut self) -> Result<Engine, String> {
        let plan = self.compile_graph().map_err(|err| err.to_string())?;
        let plugins = &self.plugins;
        let plots = &mut self.execution.plots;
        let processors = processors_for(&plan, &[], |step| make_processor(plugins, plots, step))
            .map_err(|err| err.to_string())?;
        Engine::new(
            plan,
            processors,
            self.sample_rate as f32,
            ONE_PASS_MAX_FRAMES,
        )
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
    use v_signal_nodes_plugin_std::{
        AMPLIFIER, DEVICE_OUTPUT, DUAL_BEAM_OSCILLOSCOPE, GENERATOR, OSCILLOSCOPE,
    };

    /// An app with source → gain → sink.
    fn runnable() -> VSignalNodesApp {
        let mut app = VSignalNodesApp::default();
        let add = |app: &mut VSignalNodesApp, type_id, x| {
            app.add_node_at(app.kind_of(type_id).unwrap(), egui::pos2(x, 0.0))
                .unwrap()
        };
        let source = add(&mut app, GENERATOR, 0.0);
        let gain = add(&mut app, AMPLIFIER, 300.0);
        let sink = add(&mut app, DEVICE_OUTPUT, 600.0);
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
            .add_node_at(app.kind_of(DEVICE_OUTPUT).unwrap(), egui::Pos2::ZERO)
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
            .add_node_at(app.kind_of(DEVICE_OUTPUT).unwrap(), egui::Pos2::ZERO)
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
            .add_node_at(app.kind_of(DEVICE_OUTPUT).unwrap(), egui::pos2(300.0, 0.0))
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

    /// Generator → oscilloscope on the real device: sweeps reach the window from the
    /// real-time thread, and without an output node nothing is heard. Ignored like the
    /// test above.
    #[test]
    #[ignore = "needs an output device"]
    fn the_real_time_thread_feeds_the_plot_window() {
        let mut app = VSignalNodesApp::default();
        let ctx = egui::Context::default();
        let source = app
            .add_node_at(app.kind_of(GENERATOR).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let scope = app
            .add_node_at(app.kind_of(OSCILLOSCOPE).unwrap(), egui::pos2(300.0, 0.0))
            .unwrap();
        assert!(app.connect(OutputRef::new(source, 0), InputRef::new(scope, 0)));
        app.refresh_compile_status();

        app.execute(Command::RunRealtime);
        assert_eq!(app.execution.failure(), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.execution.plot_frame(scope, 0).is_none() {
            assert!(Instant::now() < deadline, "no sweep reached the window");
            app.update_execution(&ctx);
            thread::sleep(Duration::from_millis(5));
        }
        app.execute(Command::RunRealtime);

        let frame = app.execution.plot_frame(scope, 0).unwrap();
        let peak = frame
            .samples
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.05, "{peak}");
    }

    /// A new sample rate while running: the device starts over, resampling whatever
    /// the project's rate is. Only an unconnected output node, so it's silent. Ignored
    /// like the test above.
    #[test]
    #[ignore = "needs an output device"]
    fn a_new_sample_rate_restarts_the_device() {
        let mut app = VSignalNodesApp::default();
        let ctx = egui::Context::default();
        app.add_node_at(app.kind_of(DEVICE_OUTPUT).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        app.refresh_compile_status();
        app.execute(Command::RunRealtime);

        for rate in [96_000, 44_100, 192_000] {
            assert!(app.set_sample_rate(rate));
            for _ in 0..5 {
                app.update_execution(&ctx);
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(app.execution.failure(), None, "{rate}");
            assert!(app.execution.is_running_realtime(), "{rate}");
        }
    }

    /// A changed parameter goes straight to the running device: no recompiling, no new
    /// plan. The gain has no input, so it's silent. Ignored like the test above.
    #[test]
    #[ignore = "needs an output device"]
    fn a_changed_parameter_needs_no_new_plan() {
        let mut app = VSignalNodesApp::default();
        let ctx = egui::Context::default();
        let gain = app
            .add_node_at(app.kind_of(AMPLIFIER).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let sink = app
            .add_node_at(app.kind_of(DEVICE_OUTPUT).unwrap(), egui::pos2(300.0, 0.0))
            .unwrap();
        assert!(app.connect(OutputRef::new(gain, 0), InputRef::new(sink, 0)));
        app.refresh_compile_status();
        app.execute(Command::RunRealtime);
        app.update_execution(&ctx);
        assert!(app.execution.is_running_realtime());
        assert!(!app.execution.plan_outdated);

        assert!(app.set_param(gain, 0, ParamValue::Number(0.5)));
        assert!(!app.execution.plan_outdated);
        app.update_execution(&ctx);
        assert!(!app.execution.plan_outdated);
        assert_eq!(app.execution.failure(), None);
    }

    #[test]
    fn a_plan_compiled_after_a_change_has_the_new_value() {
        let mut app = runnable();
        let gain = app
            .nodes()
            .find(|node| node.node.type_id() == AMPLIFIER)
            .unwrap()
            .id;

        assert!(app.set_param(gain, 0, ParamValue::Number(0.5)));

        let plan = app.compile_graph().unwrap();
        let step = plan.steps().iter().find(|step| step.node == gain).unwrap();
        assert_eq!(step.params[0], ParamValue::Number(0.5));
    }

    /// A voltage on the screen of an oscilloscope, without any device: a one-pass run
    /// of generator → oscilloscope, with no output node.
    #[test]
    fn a_run_reaches_the_plot_window() {
        let mut app = VSignalNodesApp::default();
        let ctx = egui::Context::default();
        let source = app
            .add_node_at(app.kind_of(GENERATOR).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let scope = app
            .add_node_at(app.kind_of(OSCILLOSCOPE).unwrap(), egui::pos2(300.0, 0.0))
            .unwrap();
        assert!(app.connect(OutputRef::new(source, 0), InputRef::new(scope, 0)));
        app.refresh_compile_status();
        assert_eq!(app.execution.plot_frame(scope, 0), None);

        app.execute(Command::RunAsync);
        let deadline = Instant::now() + Duration::from_secs(60);
        while app.execution.is_running_one_pass() {
            assert!(Instant::now() < deadline, "the run doesn't finish");
            app.update_execution(&ctx);
            thread::sleep(Duration::from_millis(5));
        }
        app.update_execution(&ctx);

        assert_eq!(app.execution.failure(), None);
        let frame = app.execution.plot_frame(scope, 0).unwrap();
        // A 1 V sine, triggered on its rising edge through 0 V.
        assert!(frame.samples[0].abs() < 0.2, "{}", frame.samples[0]);
        let peak = frame
            .samples
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.05, "{peak}");

        app.remove_node(scope);
        app.update_execution(&ctx);
        assert_eq!(app.execution.plot_frame(scope, 0), None);
    }

    /// Two generators into the dual-beam oscilloscope, once: both beams reach its
    /// window in one frame, the second with its own amplitude.
    #[test]
    fn both_beams_of_a_dual_beam_oscilloscope_reach_the_window() {
        let mut app = VSignalNodesApp::default();
        let first = app
            .add_node_at(app.kind_of(GENERATOR).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let second = app
            .add_node_at(app.kind_of(GENERATOR).unwrap(), egui::pos2(0.0, 300.0))
            .unwrap();
        let scope = app
            .add_node_at(
                app.kind_of(DUAL_BEAM_OSCILLOSCOPE).unwrap(),
                egui::pos2(300.0, 0.0),
            )
            .unwrap();
        let amplitude = app
            .graph
            .node(second)
            .unwrap()
            .spec()
            .param_index("amplitude")
            .unwrap();
        assert!(app.set_param(second, amplitude, ParamValue::Number(3.0)));
        assert!(app.connect(OutputRef::new(first, 0), InputRef::new(scope, 0)));
        assert!(app.connect(OutputRef::new(second, 0), InputRef::new(scope, 1)));

        run_once(&mut app);

        let frame = app.execution.plot_frame(scope, 0).unwrap();
        assert_eq!(frame.beams, 2);
        let peaks: Vec<f32> = frame
            .beams()
            .map(|beam| beam.iter().fold(0.0_f32, |peak, s| peak.max(s.abs())))
            .collect();
        assert!((peaks[0] - 1.0).abs() < 0.05, "{peaks:?}");
        assert!((peaks[1] - 3.0).abs() < 0.15, "{peaks:?}");
    }

    /// Another graph's nodes may have the ids of the old ones: the old nodes' plots
    /// must not show up in their windows.
    #[test]
    fn opening_a_document_forgets_the_plots() {
        let mut app = VSignalNodesApp::default();
        let source = app
            .add_node_at(app.kind_of(GENERATOR).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let scope = app
            .add_node_at(app.kind_of(OSCILLOSCOPE).unwrap(), egui::pos2(300.0, 0.0))
            .unwrap();
        assert!(app.connect(OutputRef::new(source, 0), InputRef::new(scope, 0)));
        let document = app.to_document();
        run_once(&mut app);
        assert!(app.execution.plot_frame(scope, 0).is_some());

        app.open_document(&document).unwrap();

        let reopened = app
            .nodes()
            .find(|node| node.node.type_id() == OSCILLOSCOPE)
            .unwrap()
            .id;
        assert_eq!(reopened, scope, "the same id in the new graph");
        assert_eq!(app.execution.plot_frame(scope, 0), None);
    }

    /// Opening a document while running at the same sample rate: the device starts
    /// over with the new graph instead of handing it the old nodes' processors.
    /// Silent: the old graph has only an unconnected output node, the new one too.
    /// Ignored because CI machines have no device.
    #[test]
    #[ignore = "needs an output device"]
    fn opening_a_document_while_running_starts_over() {
        let mut app = VSignalNodesApp::default();
        let ctx = egui::Context::default();
        app.add_node_at(app.kind_of(DEVICE_OUTPUT).unwrap(), egui::Pos2::ZERO)
            .unwrap();
        let document = app.to_document();
        app.refresh_compile_status();
        app.execute(Command::RunRealtime);
        assert!(app.execution.is_running_realtime());

        app.open_document(&document).unwrap();

        assert!(app.execution.is_running_realtime());
        assert!(
            !app.execution.plan_outdated,
            "started over with the new graph"
        );
        for _ in 0..5 {
            app.update_execution(&ctx);
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(app.execution.failure(), None);
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

    /// Runs `app` once to the end and returns what came of it.
    fn run_once(app: &mut VSignalNodesApp) -> OnePassSummary {
        let ctx = egui::Context::default();
        app.execute(Command::RunAsync);
        let deadline = Instant::now() + Duration::from_secs(60);
        while app.execution.is_running_one_pass() {
            assert!(Instant::now() < deadline, "the run doesn't finish");
            app.update_execution(&ctx);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.execution.failure(), None);
        app.execution.one_pass_result().unwrap()
    }

    #[test]
    fn a_run_goes_at_the_projects_sample_rate() {
        let mut app = runnable();
        let source = app
            .nodes()
            .find(|node| node.node.type_id() == GENERATOR)
            .unwrap()
            .id;
        let frequency = app
            .graph
            .node(source)
            .unwrap()
            .spec()
            .param_index("frequency")
            .unwrap();
        assert!(app.set_param(source, frequency, ParamValue::Number(30_000.0)));
        assert_eq!(app.sample_rate(), 48_000);

        let peak = run_once(&mut app).peak;
        assert!(
            peak < 1e-3,
            "above half of 48 kHz the generator stops: {peak}"
        );

        assert!(app.set_sample_rate(96_000));
        let peak = run_once(&mut app).peak;
        assert!(peak > 0.9, "below half of 96 kHz: {peak}");
    }

    #[test]
    fn frequencies_too_high_for_the_sample_rate_are_found() {
        let mut app = runnable();
        let source = app
            .nodes()
            .find(|node| node.node.type_id() == GENERATOR)
            .unwrap()
            .id;
        let frequency = app
            .graph
            .node(source)
            .unwrap()
            .spec()
            .param_index("frequency")
            .unwrap();
        assert_eq!(app.few_samples_per_period().count(), 0, "1 kHz at 48 kHz");

        // A quarter of 48 kHz is still enough; above it isn't.
        assert!(app.set_param(source, frequency, ParamValue::Number(12_000.0)));
        assert_eq!(app.few_samples_per_period().count(), 0);
        assert!(app.set_param(source, frequency, ParamValue::Number(12_001.0)));
        assert_eq!(
            app.few_samples_per_period().collect::<Vec<_>>(),
            [(source, frequency, 12_001.0)]
        );

        // Only parameters in hertz count: a large amplitude doesn't.
        assert!(app.set_param(source, frequency, ParamValue::Number(10_000.0)));
        let amplitude = app
            .graph
            .node(source)
            .unwrap()
            .spec()
            .param_index("amplitude")
            .unwrap();
        assert!(app.set_param(source, amplitude, ParamValue::Number(1000.0)));
        assert_eq!(app.few_samples_per_period().count(), 0);

        assert!(app.set_sample_rate(32_000));
        assert_eq!(app.few_samples_per_period().count(), 1, "a lower rate");
    }

    #[test]
    fn sample_rates_out_of_range_are_refused() {
        let mut app = VSignalNodesApp::default();
        for rate in [0, 999, 10_000_001] {
            assert!(!app.set_sample_rate(rate), "{rate}");
        }
        assert_eq!(app.sample_rate(), 48_000);
    }
}

//! Bottom debug panel.
//!
//! Shows, top to bottom:
//! - a node-limit warning when the last add failed (see `node_limit_reached`);
//! - a warning when the last connection was refused, with the reason (a cycle or
//!   sockets that don't fit, `connection_refused`);
//! - why the graph can't run, before anyone tries (nothing for an empty canvas: there
//!   is nothing to run yet), and what the last run came to (see `editor::execution`);
//! - the selected source socket, or "Ready" when nothing is selected and there
//!   are no warnings;
//! - every connection as `from_node Output n -> to_node Input m`, sorted by source
//!   and scrollable once it exceeds [`CONNECTIONS_MAX_HEIGHT`].
//!
//! Nodes are shown by their number (the `#n` of the node title, 0-based), socket
//! numbers 1-based. Read-only: the panel never changes the app state.

use eframe::egui;
use v_signal_nodes_core::CompileError;

use super::{TEXT_DIM, caption, panel_frame};
use crate::editor::{Language, MAX_NODE_COUNT, SocketRef, Text, VSignalNodesApp};

/// Height after which the connection list scrolls instead of growing the panel.
const CONNECTIONS_MAX_HEIGHT: f32 = 120.0;

/// Shows the debug panel along the bottom of the window.
pub(super) fn show(app: &VSignalNodesApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("debug")
        .resizable(false)
        .frame(panel_frame(super::PANEL_BG))
        .show(ui, |ui| {
            caption(ui, app.language.text(Text::Debug));
            if app.node_limit_reached {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!(
                        "{}: {}",
                        app.language.text(Text::NodeLimitReached),
                        MAX_NODE_COUNT,
                    ),
                );
            }
            if let Some(reason) = app.connection_refused {
                ui.colored_label(ui.visuals().error_fg_color, app.language.text(reason));
            }
            show_execution(app, ui);
            if let Some(source) = app.pending_source {
                ui.label(format!(
                    "{} {}",
                    app.language.text(Text::SelectedSourceSocket),
                    socket_text(SocketRef::Output(source), app.language),
                ));
            } else if !app.node_limit_reached && app.connection_refused.is_none() {
                ui.label(app.language.text(Text::Ready));
            }
            egui::ScrollArea::vertical()
                .max_height(CONNECTIONS_MAX_HEIGHT)
                .show(ui, |ui| {
                    for (from, to) in app.sorted_connections() {
                        let line = format!(
                            "{} -> {}",
                            socket_text(SocketRef::Output(from), app.language),
                            socket_text(SocketRef::Input(to), app.language),
                        );
                        ui.label(
                            egui::RichText::new(line)
                                .monospace()
                                .size(12.0)
                                .color(TEXT_DIM),
                        );
                    }
                });
        });
}

/// Why the graph can't run and how the last run went.
fn show_execution(app: &VSignalNodesApp, ui: &mut egui::Ui) {
    let execution = &app.execution;
    let language = app.language;
    match execution.compile_error() {
        // Normal while building a graph, so not shown as an error.
        Some(CompileError::NoOutput) if !app.graph.is_empty() => {
            ui.colored_label(TEXT_DIM, language.text(Text::NoOutputNode));
        }
        Some(error @ CompileError::Incompatible { .. }) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("{}: {error}", language.text(Text::GraphCantRun)),
            );
        }
        _ => {}
    }
    if execution.is_running_realtime() {
        ui.label(language.text(Text::RunningRealtime));
    }
    if execution.is_running_one_pass() {
        ui.label(language.text(Text::RunningOnePass));
    }
    if let Some(summary) = execution.one_pass_result() {
        ui.label(format!(
            "{}: {} / {:.0} ms / {:.3}",
            language.text(Text::OnePassResult),
            summary.frames,
            summary.elapsed.as_secs_f64() * 1000.0,
            summary.peak,
        ));
    }
    if let Some(failure) = execution.failure() {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("{}: {failure}", language.text(Text::RunFailed)),
        );
    }
}

/// `socket` as `node Side n`, e.g. `3 Output 1` (node number as in the node
/// title, socket number 1-based).
fn socket_text(socket: SocketRef, language: Language) -> String {
    format!(
        "{} {} {}",
        socket.node(),
        language.text(socket.side().into()),
        socket.index() + 1
    )
}

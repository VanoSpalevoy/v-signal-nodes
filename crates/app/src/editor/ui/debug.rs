//! Bottom debug panel.
//!
//! Shows, top to bottom:
//! - a node-limit warning when the last add failed (see `node_limit_reached`);
//! - a warning when the last connection was refused as a cycle (`connection_refused`);
//! - the selected source socket, or "Ready" when nothing is selected and there
//!   are no warnings;
//! - every connection as `from_node Output n -> to_node Input m`, sorted by source
//!   and scrollable once it exceeds [`CONNECTIONS_MAX_HEIGHT`].
//!
//! Node IDs are shown as-is (0-based), socket numbers 1-based. Read-only: the
//! panel never changes the app state.

use eframe::egui;

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
            if app.connection_refused {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    app.language.text(Text::ConnectionCreatesCycle),
                );
            }
            if let Some(source) = app.pending_source {
                ui.label(format!(
                    "{} {}",
                    app.language.text(Text::SelectedSourceSocket),
                    socket_text(source, app.language),
                ));
            } else if !app.node_limit_reached && !app.connection_refused {
                ui.label(app.language.text(Text::Ready));
            }
            egui::ScrollArea::vertical()
                .max_height(CONNECTIONS_MAX_HEIGHT)
                .show(ui, |ui| {
                    for (from, to) in app.sorted_connections() {
                        let line = format!(
                            "{} -> {}",
                            socket_text(from, app.language),
                            socket_text(to, app.language),
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

/// `socket` as `node Side n`, e.g. `3 Output 1` (socket number 1-based).
fn socket_text(socket: SocketRef, language: Language) -> String {
    format!(
        "{} {} {}",
        socket.node_id,
        socket.side.label(language),
        socket.index + 1
    )
}

//! The windows of nodes.
//!
//! A node's windows ([`NodeWindow`]) open and close with the buttons in its header.
//! This module is the frame around them and what's common to all: which are open
//! (`app.open_windows`), title, closing. What they hold is up to [`params`] (advanced
//! parameters) and [`plot`] (windows a plugin declared, by their [`WindowContent`]).
//!
//! There are two sorts of window:
//!
//! - **Advanced parameters** and **previews** of declared windows float above the
//!   canvas, inside the main window.
//! - **Separate windows** show a declared window as a window of the system, independent
//!   of the main one: they go beyond it and can be above or behind it, and be as big as
//!   the screen. Each is an egui viewport that runs and repaints by itself (deferred),
//!   so it keeps moving also while the main window is covered and doesn't repaint.
//!   Being separate, it only gets copies of what it shows; closing it comes back through
//!   `app.closed_windows`, and the main window then stops showing it. Where a backend
//!   has no such windows (and in tests), egui shows it inside the main window instead.
//!
//! A preview and a separate window of the same declared window open independently and
//! show the same frames.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eframe::egui;
use v_signal_nodes_core::{AxisUnit, NodeId, WindowContent};

use super::{NodeWidget, NodeWindow, PANEL_BG, header_fill, panel_frame, params, plot};
use crate::editor::execution::{PlotFeed, read_feed};
use crate::editor::{Language, VSignalNodesApp};

/// Size a plot's separate window first opens with.
const PLOT_DEFAULT_SIZE: egui::Vec2 = egui::vec2(480.0, 300.0);
/// Smallest size of a plot's separate window.
const PLOT_MIN_SIZE: egui::Vec2 = egui::vec2(280.0, 180.0);
/// Size a plot's preview first opens with.
const PREVIEW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(320.0, 200.0);
/// Smallest size of a plot's preview.
const PREVIEW_MIN_SIZE: egui::Vec2 = egui::vec2(240.0, 150.0);
/// How often a plot window repaints while its node can publish.
const PLOT_REFRESH_INTERVAL: Duration = Duration::from_millis(33);

/// Declared windows the user closed, waiting for the main window to take notice.
pub(in crate::editor) type ClosedWindows = Arc<Mutex<Vec<(NodeId, NodeWindow)>>>;

/// Shows every open window and applies what was changed in them.
pub(super) fn show(app: &mut VSignalNodesApp, ui: &egui::Ui) {
    let closed_by_user = std::mem::take(
        &mut *app
            .closed_windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    for (node_id, window) in closed_by_user {
        app.close_window(node_id, window);
    }

    let language = app.language;
    let mut edits = Vec::new();
    let mut closed = Vec::new();
    // Only the first frame of a window uses it: it opens where its button was clicked.
    let pointer = ui.input(|input| input.pointer.latest_pos());
    for &(node_id, kind) in &app.open_windows {
        let Some(node) = app.node(node_id) else {
            continue;
        };
        let mut open = true;
        let floating = |open| {
            let window = egui::Window::new(window_title(&node, kind, language))
                .id(window_id(node_id, kind))
                .open(open)
                .collapsible(false);
            match pointer {
                Some(pointer) => window.default_pos(pointer),
                None => window,
            }
        };
        match kind {
            NodeWindow::AdvancedParams => {
                let window = floating(&mut open).resizable(false);
                show_floating(ui.ctx(), window, &node, |ui| {
                    let changed = params::advanced_window(ui, &node, language);
                    edits.extend(changed.into_iter().map(|edit| (node_id, edit)));
                });
            }
            NodeWindow::Preview(index) | NodeWindow::Separate(index) => {
                match node.spec().windows[index].content {
                    WindowContent::Plot { x, y } => {
                        let contents = PlotContents {
                            feed: app.execution.plot_feed(node_id, index),
                            x,
                            y,
                            language,
                        };
                        if matches!(kind, NodeWindow::Preview(_)) {
                            let window = floating(&mut open)
                                .default_size(PREVIEW_DEFAULT_SIZE)
                                .min_size(PREVIEW_MIN_SIZE);
                            show_floating(ui.ctx(), window, &node, |ui| contents.show(ui));
                        } else {
                            let plot = PlotWindow {
                                window: (node_id, kind),
                                contents,
                                closed: Arc::clone(&app.closed_windows),
                            };
                            plot.show(ui.ctx(), &node);
                        }
                    }
                }
            }
        }
        if !open {
            closed.push((node_id, kind));
        }
    }
    for (node_id, (index, value)) in edits {
        app.set_param(node_id, index, value);
    }
    for (node_id, kind) in closed {
        app.close_window(node_id, kind);
    }
}

/// Shows a floating `window` of `node` with its title bar in the node's header
/// color, so it shows whose it is. Also on top: egui paints the title bar of the
/// topmost window with the fill of open widgets, so that fill is swapped while the
/// title bar is painted, and the contents get the theme as it is.
fn show_floating(
    ctx: &egui::Context,
    window: egui::Window,
    node: &NodeWidget,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let theme = ctx.global_style();
    let fill = header_fill(node.spec());
    let mut titled = (*theme).clone();
    titled.visuals.widgets.open.weak_bg_fill = fill;
    ctx.set_global_style(titled);
    window
        .title_frame(egui::Frame::window(&theme).fill(fill))
        .show(ctx, |ui| {
            ctx.set_global_style(Arc::clone(&theme));
            ui.set_style(Arc::clone(&theme));
            add_contents(ui);
        });
    ctx.set_global_style(theme);
}

/// The id of a floating window of a node.
fn window_id(node_id: NodeId, kind: NodeWindow) -> egui::Id {
    egui::Id::unique(("node_window", node_id, kind))
}

/// The title of a window of `node` in `language`.
fn window_title(node: &NodeWidget, kind: NodeWindow, language: Language) -> String {
    let (title, id) = (node.title(language), node.id);
    match kind {
        NodeWindow::AdvancedParams => format!("{title} #{id}"),
        NodeWindow::Preview(_) | NodeWindow::Separate(_) => {
            format!("{} — {title} #{id}", node.window_name(kind, language))
        }
    }
}

/// What a plot's window draws, wherever it is.
struct PlotContents {
    feed: Option<PlotFeed>,
    x: AxisUnit,
    y: AxisUnit,
    language: Language,
}

impl PlotContents {
    /// Takes in the newest frame and draws it; asks for the next repaint while more
    /// can come.
    fn show(&self, ui: &mut egui::Ui) {
        let Some(feed) = &self.feed else {
            plot::show(ui, None, self.x, self.y, self.language);
            return;
        };
        let mut reader = read_feed(feed);
        reader.poll();
        plot::show(ui, reader.latest(), self.x, self.y, self.language);
        if reader.is_live() {
            ui.ctx().request_repaint_after(PLOT_REFRESH_INTERVAL);
        }
    }
}

/// What the system window of a plot needs, as it runs apart from the app.
struct PlotWindow {
    window: (NodeId, NodeWindow),
    contents: PlotContents,
    closed: ClosedWindows,
}

impl PlotWindow {
    /// Keeps the window of `node` open for this frame of the main window, with the
    /// newest feed, and has it repaint, as a run may have just started or ended.
    fn show(self, ctx: &egui::Context, node: &NodeWidget) {
        let (node_id, kind) = self.window;
        let id = egui::ViewportId::from_hash_of(("node_window", node_id, kind));
        let builder = egui::ViewportBuilder::default()
            .with_title(window_title(node, kind, self.contents.language))
            .with_inner_size(PLOT_DEFAULT_SIZE)
            .with_min_inner_size(PLOT_MIN_SIZE);
        ctx.show_viewport_deferred(id, builder, move |ui, class| {
            if class == egui::ViewportClass::EmbeddedWindow {
                self.contents.show(ui);
                return;
            }
            egui::CentralPanel::default()
                .frame(panel_frame(PANEL_BG))
                .show(ui, |ui| self.contents.show(ui));
            self.handle_close(ui.ctx());
        });
        ctx.request_repaint_of(id);
    }

    /// If the user closes the window: hides it at once and tells the main window,
    /// which ends it when it next runs. It may be covered and not run for a while.
    fn handle_close(&self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().close_requested()) {
            self.closed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(self.window);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::tests::add;
    use crate::editor::ui::headless::{self, Frames};
    use crate::editor::ui::{install_theme, render};
    use v_signal_nodes_core::{NodeId, ParamValue};
    use v_signal_nodes_plugin_std::OSCILLOGRAPH;

    /// The editor without a window: whole frames, with pointer events.
    struct Editor {
        frames: Frames,
        app: VSignalNodesApp,
    }

    impl Editor {
        /// An editor with an oscillograph, its window `kind` opened with the pointer
        /// at `at`.
        fn with_window(at: egui::Pos2, kind: NodeWindow) -> (Self, NodeId) {
            let frames = Frames::new();
            install_theme(&frames.ctx);
            let mut app = VSignalNodesApp::default();
            let scope = add(&mut app, OSCILLOGRAPH, egui::pos2(500.0, 300.0));
            let mut editor = Self { frames, app };
            editor.frame(vec![egui::Event::PointerMoved(at)]);
            editor.app.toggle_window(scope, kind);
            editor.frame(Vec::new());
            editor.frame(Vec::new());
            (editor, scope)
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let app = &mut self.app;
            self.frames.run(events, |ui| render(app, ui))
        }

        fn click(&mut self, pos: egui::Pos2) {
            for events in headless::click(pos) {
                self.frame(events);
            }
        }
    }

    #[test]
    fn the_advanced_window_opens_at_the_pointer_and_edits_the_node() {
        let at = egui::pos2(100.0, 300.0);
        let (mut editor, scope) = Editor::with_window(at, NodeWindow::AdvancedParams);
        assert_eq!(
            editor.app.graph.node(scope).unwrap().param("trigger"),
            Some(ParamValue::Choice(1))
        );

        // Trigger, the first control, below the title and the caption.
        let trigger = at + egui::vec2(60.0, 71.0);
        editor.click(trigger);
        // Its first option, "free run", right below it.
        editor.click(trigger + egui::vec2(0.0, 36.0));

        assert_eq!(
            editor.app.graph.node(scope).unwrap().param("trigger"),
            Some(ParamValue::Choice(0))
        );
    }

    #[test]
    fn a_preview_opens_at_the_pointer_holds_its_size_and_closes_with_its_cross() {
        let at = egui::pos2(100.0, 200.0);
        let (mut editor, scope) = Editor::with_window(at, NodeWindow::Preview(0));
        let area = |editor: &Editor| {
            let id = window_id(scope, NodeWindow::Preview(0));
            editor.frames.ctx.memory(|memory| memory.area_rect(id))
        };

        let rect = area(&editor).expect("the preview is shown");
        assert_eq!(rect.min, at);
        for _ in 0..5 {
            editor.frame(Vec::new());
        }
        assert_eq!(area(&editor), Some(rect), "contents don't make it grow");

        editor.click(rect.right_top() + egui::vec2(-16.0, 16.0));

        assert!(editor.app.open_windows.is_empty());
    }

    #[test]
    fn floating_windows_have_their_nodes_header_color_also_on_top() {
        for kind in [NodeWindow::AdvancedParams, NodeWindow::Preview(0)] {
            let at = egui::pos2(100.0, 200.0);
            let (mut editor, scope) = Editor::with_window(at, kind);
            let fill = header_fill(editor.app.node(scope).unwrap().spec());
            // Past the fade-in, which dims every color of the window.
            for _ in 0..20 {
                editor.frame(Vec::new());
            }
            let output = editor.frame(Vec::new());

            let window = editor
                .frames
                .ctx
                .memory(|memory| memory.area_rect(window_id(scope, kind)))
                .unwrap();
            let title_bar = output.shapes.iter().any(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) => rect.fill == fill && rect.rect.min == window.min,
                _ => false,
            });
            assert!(title_bar, "{kind:?}");
        }
    }

    #[test]
    fn a_plot_window_shows_without_data_and_goes_with_its_node() {
        let at = egui::pos2(100.0, 300.0);
        let (mut editor, scope) = Editor::with_window(at, NodeWindow::Separate(0));
        assert_eq!(editor.app.open_windows, [(scope, NodeWindow::Separate(0))]);

        editor.app.remove_node(scope);
        editor.frame(Vec::new());

        assert!(editor.app.open_windows.is_empty());
    }

    #[test]
    fn a_window_closed_by_the_user_is_closed_in_the_next_frame() {
        let at = egui::pos2(100.0, 300.0);
        let (mut editor, scope) = Editor::with_window(at, NodeWindow::Separate(0));

        let window = (scope, NodeWindow::Separate(0));
        editor.app.closed_windows.lock().unwrap().push(window);
        editor.frame(Vec::new());

        assert!(editor.app.open_windows.is_empty());
        assert!(editor.app.closed_windows.lock().unwrap().is_empty());
    }
}

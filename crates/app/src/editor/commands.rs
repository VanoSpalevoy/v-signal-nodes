//! Editor commands: everything the user can invoke from the menu bar or with a
//! keyboard shortcut.
//!
//! [`Command`] is the single table of commands: its label, shortcuts and whether
//! it is available now. The UI only presents it (the menu bar, the shortcut
//! handling in `ui::menu`, toolbar buttons) and runs commands through
//! [`VSignalNodesApp::execute`], so a command behaves the same however it was
//! invoked, and adding one is a variant plus its match arms here.
//!
//! Clipboard commands are the exception: the clipboard belongs to egui, so
//! `ui::menu` does the clipboard I/O and calls
//! [`copy_selection`](VSignalNodesApp::copy_selection) /
//! [`paste`](VSignalNodesApp::paste) for the graph side.

use eframe::egui::{Key, KeyboardShortcut, Modifiers};

use super::{Text, VSignalNodesApp};

/// Grid size used by Snap to grid, in canvas units.
pub(super) const GRID_STEP: f32 = 50.0;
/// Allowed canvas zoom.
pub(super) const SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// Zoom step of the toolbar slider and the zoom commands. Coarse steps keep the
/// number of distinct font sizes (each one is rasterized into egui's font atlas) small.
pub(super) const SCALE_STEP: f32 = 0.1;

/// A user command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    /// Copies the selected nodes to the clipboard.
    Copy,
    /// Pastes nodes from the clipboard.
    Paste,
    /// Snaps every node to a [`GRID_STEP`] grid.
    SnapToGrid,
    /// Zooms the canvas in by [`SCALE_STEP`].
    ZoomIn,
    /// Zooms the canvas out by [`SCALE_STEP`].
    ZoomOut,
    /// Resets the canvas zoom to 1.
    ResetZoom,
    /// Starts or stops real-time processing on the output device.
    RunRealtime,
    /// Runs the graph once on background threads, independent branches in parallel.
    RunAsync,
}

/// A top-level menu of the menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Edit,
    View,
    Run,
}

impl Menu {
    /// Menus in menu bar order.
    pub(super) const ALL: [Self; 3] = [Self::Edit, Self::View, Self::Run];

    /// The menu's title.
    pub(super) fn text(self) -> Text {
        match self {
            Self::Edit => Text::MenuEdit,
            Self::View => Text::MenuView,
            Self::Run => Text::MenuRun,
        }
    }

    /// The menu's commands, in display order.
    pub(super) fn commands(self) -> &'static [Command] {
        match self {
            Self::Edit => &[Command::Copy, Command::Paste],
            Self::View => &[
                Command::ZoomIn,
                Command::ZoomOut,
                Command::ResetZoom,
                Command::SnapToGrid,
            ],
            Self::Run => &[Command::RunRealtime, Command::RunAsync],
        }
    }
}

impl Command {
    /// The command's label.
    pub(super) fn text(self) -> Text {
        match self {
            Self::Copy => Text::Copy,
            Self::Paste => Text::Paste,
            Self::SnapToGrid => Text::SnapToGrid,
            Self::ZoomIn => Text::ZoomIn,
            Self::ZoomOut => Text::ZoomOut,
            Self::ResetZoom => Text::ResetZoom,
            Self::RunRealtime => Text::RunRealtime,
            Self::RunAsync => Text::RunAsync,
        }
    }

    /// Keyboard shortcuts of the command, the first one shown in the menu. Empty
    /// if it has none. [`Modifiers::COMMAND`] is Cmd on macOS and Ctrl elsewhere.
    ///
    /// Copy and Paste reach egui as clipboard events, not key presses, so their
    /// shortcuts are only shown here; `ui::menu` handles the events.
    pub(super) fn shortcuts(self) -> &'static [KeyboardShortcut] {
        const fn key(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(modifiers, key)
        }
        // `const` blocks so the arrays are promoted to statics.
        match self {
            Self::Copy => const { &[key(Modifiers::COMMAND, Key::C)] },
            Self::Paste => const { &[key(Modifiers::COMMAND, Key::V)] },
            Self::SnapToGrid => &[],
            // `+` needs Shift on many layouts, so `=` (the same key) works too.
            Self::ZoomIn => {
                const {
                    &[
                        key(Modifiers::COMMAND, Key::Plus),
                        key(Modifiers::COMMAND, Key::Equals),
                    ]
                }
            }
            Self::ZoomOut => const { &[key(Modifiers::COMMAND, Key::Minus)] },
            Self::ResetZoom => const { &[key(Modifiers::COMMAND, Key::Num0)] },
            Self::RunRealtime => const { &[key(Modifiers::NONE, Key::Space)] },
            Self::RunAsync => const { &[key(Modifiers::COMMAND, Key::Enter)] },
        }
    }
}

impl VSignalNodesApp {
    /// Whether `command` can run now; the menu greys out the others and their
    /// shortcuts do nothing.
    pub(super) fn can_execute(&self, command: Command) -> bool {
        match command {
            // No node selection yet (TODO: multi-selection), so nothing to copy or paste.
            Command::Copy | Command::Paste => false,
            Command::SnapToGrid => !self.graph.is_empty(),
            Command::ZoomIn => self.scale < *SCALE_RANGE.end(),
            Command::ZoomOut => self.scale > *SCALE_RANGE.start(),
            Command::ResetZoom => self.scale != 1.0,
            // Stopping is always possible.
            Command::RunRealtime => {
                self.execution.is_running_realtime() || self.execution.compile_error().is_none()
            }
            Command::RunAsync => {
                !self.execution.is_running_one_pass() && self.execution.compile_error().is_none()
            }
        }
    }

    /// Runs `command` if [`can_execute`](Self::can_execute) allows it. Clipboard
    /// commands do nothing here: see the module docs.
    pub(super) fn execute(&mut self, command: Command) {
        self.refresh_compile_status();
        if !self.can_execute(command) {
            return;
        }
        match command {
            Command::Copy | Command::Paste => {}
            Command::SnapToGrid => self.snap_to_grid(GRID_STEP),
            Command::ZoomIn => self.set_scale(self.scale + SCALE_STEP),
            Command::ZoomOut => self.set_scale(self.scale - SCALE_STEP),
            Command::ResetZoom => self.set_scale(1.0),
            Command::RunRealtime => self.toggle_realtime(),
            Command::RunAsync => self.start_one_pass(),
        }
    }

    /// Sets the canvas zoom to `scale`, rounded to a [`SCALE_STEP`] multiple (so
    /// repeated steps don't accumulate float error) and clamped to [`SCALE_RANGE`].
    fn set_scale(&mut self, scale: f32) {
        let steps = (scale / SCALE_STEP).round();
        self.scale = (steps * SCALE_STEP).clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end());
    }

    /// The selected nodes as clipboard text, or `None` if nothing is selected.
    ///
    /// Not implemented yet: there is no node selection. The plan is a
    /// [`Document`](crate::document::Document) of the selected nodes and the
    /// connections between them, as JSON.
    pub(super) fn copy_selection(&self) -> Option<String> {
        None
    }

    /// Pastes clipboard `text` copied by [`copy_selection`](Self::copy_selection)
    /// as new nodes. Not implemented yet; see there.
    pub(super) fn paste(&mut self, text: &str) {
        let _ = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_in_exactly_one_menu() {
        let all = [
            Command::Copy,
            Command::Paste,
            Command::SnapToGrid,
            Command::ZoomIn,
            Command::ZoomOut,
            Command::ResetZoom,
            Command::RunRealtime,
            Command::RunAsync,
        ];
        for command in all {
            let menus = Menu::ALL
                .iter()
                .filter(|menu| menu.commands().contains(&command))
                .count();
            assert_eq!(menus, 1, "{command:?}");
        }
    }

    #[test]
    fn shortcuts_are_unique() {
        let shortcuts: Vec<_> = Menu::ALL
            .iter()
            .flat_map(|menu| menu.commands())
            .flat_map(|command| command.shortcuts())
            .collect();
        for (i, a) in shortcuts.iter().enumerate() {
            assert!(!shortcuts[i + 1..].contains(a), "{a:?} is used twice");
        }
    }

    #[test]
    fn zoom_steps_stay_on_the_grid_and_in_range() {
        let mut app = VSignalNodesApp::default();
        for _ in 0..30 {
            app.execute(Command::ZoomIn);
        }
        assert_eq!(app.scale, *SCALE_RANGE.end());
        assert!(!app.can_execute(Command::ZoomIn));

        for _ in 0..3 {
            app.execute(Command::ZoomOut);
        }
        assert_eq!(app.scale, 1.7);

        app.execute(Command::ResetZoom);
        assert_eq!(app.scale, 1.0);
        assert!(!app.can_execute(Command::ResetZoom));

        for _ in 0..30 {
            app.execute(Command::ZoomOut);
        }
        assert_eq!(app.scale, *SCALE_RANGE.start());
    }

    #[test]
    fn unavailable_commands_do_nothing() {
        let mut app = VSignalNodesApp::default();
        assert!(!app.can_execute(Command::SnapToGrid), "no nodes");
        assert!(!app.can_execute(Command::RunRealtime), "no output node");
        assert!(!app.can_execute(Command::Copy), "no selection");
        app.execute(Command::RunRealtime);
        assert_eq!(app.copy_selection(), None);
    }
}

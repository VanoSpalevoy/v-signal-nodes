//! This module contains the UI for the time machine feature in the editor.
//! Time machine allows users to discover changes of signal values over time in each node or connection.
//! After allowing time machine in settings user can click on the time machine button on node or click on connection to open the time machine window.

use eframe::egui;

use crate::editor::{Text, VSignalNodesApp};
pub fn show_time_machine_window(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    if let Some(time_machine) = &mut app.time_machine {
        egui::Window::new(Text::TIME_MACHINE_WINDOW_TITLE)
            .open(&mut time_machine.open)
            .resizable(true)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                time_machine.ui(ui, app);
            });
    }
}
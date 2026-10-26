//! v-signal-nodes editor binary: opens the native window and runs [`editor::VSignalNodesApp`].
//!
//! Release builds on Windows use the GUI subsystem, so no console window appears.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod document;
mod editor;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        // Below this the panels squeeze the canvas to nothing.
        viewport: eframe::egui::ViewportBuilder::default().with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "v-signal-nodes",
        options,
        Box::new(|creation_context| Ok(Box::new(editor::VSignalNodesApp::new(creation_context)))),
    )
}

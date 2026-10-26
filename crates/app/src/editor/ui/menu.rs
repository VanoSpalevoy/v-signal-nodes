//! Menu bar and keyboard shortcuts: the two ways to run a [`Command`].
//!
//! Both read the command table in `editor::commands` ([`Menu::ALL`],
//! [`Command::shortcuts`]), so the menu shows exactly the shortcuts that work.
//! Commands run through [`run`], which does the clipboard I/O for Copy/Paste and
//! hands everything else to `VSignalNodesApp::execute`.
//!
//! [`handle_shortcuts`] runs first in the frame, before any widget, so a command's
//! keys are consumed before a focused widget can react to them (e.g. Space
//! clicking a focused button). Nothing is consumed while a text field has focus:
//! there the keys (and Copy/Paste) belong to the text.
//!
//! Copy and Paste don't arrive as key presses: egui turns Cmd/Ctrl+C and V into
//! [`egui::Event::Copy`] / [`egui::Event::Paste`] (the latter with the clipboard
//! text). Paste from the menu therefore asks egui for a paste
//! ([`egui::ViewportCommand::RequestPaste`]) and the text comes back as such an
//! event on a later frame.
//!
//! On macOS the menus are in the system menu bar instead (`editor::native_menu`)
//! and [`show`] draws nothing; the shortcut handling here still runs for the keys
//! the system menu leaves to the window.

use eframe::egui;

use super::panel_frame;
use crate::editor::VSignalNodesApp;
use crate::editor::commands::{Command, Menu};

/// Shows the menu bar as a top panel and runs the commands clicked in it.
/// Nothing when the menus are in the system menu bar (macOS).
pub(super) fn show(app: &mut VSignalNodesApp, ui: &mut egui::Ui) {
    if app.has_native_menu() {
        return;
    }
    egui::Panel::top("menu_bar")
        .resizable(false)
        .frame(panel_frame(super::PANEL_BG))
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for menu in Menu::ALL {
                    ui.menu_button(app.language.text(menu.text()), |ui| {
                        for &command in menu.commands() {
                            let mut button = egui::Button::new(app.language.text(command.text()));
                            if let Some(shortcut) = command.shortcuts().first() {
                                button = button.shortcut_text(ui.ctx().format_shortcut(shortcut));
                            }
                            if ui.add_enabled(app.can_execute(command), button).clicked() {
                                run(app, ui.ctx(), command);
                            }
                        }
                    });
                }
            });
        });
}

/// Runs the commands whose shortcuts were pressed this frame, and Copy/Paste on
/// egui's clipboard events. Does nothing while a text field has focus.
pub(super) fn handle_shortcuts(app: &mut VSignalNodesApp, ctx: &egui::Context) {
    if ctx.text_edit_focused() {
        return;
    }
    for &command in Menu::ALL.iter().flat_map(|menu| menu.commands()) {
        if !matches!(command, Command::Copy | Command::Paste)
            && command
                .shortcuts()
                .iter()
                .any(|shortcut| ctx.input_mut(|input| input.consume_shortcut(shortcut)))
        {
            run(app, ctx, command);
        }
    }
    let mut copy = false;
    let mut pasted = None;
    ctx.input_mut(|input| {
        input.events.retain(|event| match event {
            egui::Event::Copy => {
                copy = true;
                false
            }
            egui::Event::Paste(text) => {
                pasted = Some(text.clone());
                false
            }
            _ => true,
        });
    });
    if copy {
        run(app, ctx, Command::Copy);
    }
    if let Some(text) = pasted
        && app.can_execute(Command::Paste)
    {
        app.paste(&text);
    }
}

/// Runs `command`, doing the clipboard part of Copy/Paste here (egui owns the
/// clipboard) and everything else through `VSignalNodesApp::execute`.
fn run(app: &mut VSignalNodesApp, ctx: &egui::Context, command: Command) {
    match command {
        Command::Copy => {
            if app.can_execute(command)
                && let Some(text) = app.copy_selection()
            {
                ctx.copy_text(text);
            }
        }
        Command::Paste => {
            if app.can_execute(command) {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
        }
        _ => app.execute(command),
    }
}

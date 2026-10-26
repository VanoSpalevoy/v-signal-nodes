//! The macOS menu bar at the top of the screen, built from the command table.
//!
//! On macOS the editor's menus ([`Menu::ALL`]) live in the system menu bar
//! instead of the egui one (`ui::menu` hides it), after the usual application
//! menu (About, Services, Hide, Quit). [`NativeMenu`] owns the `muda` menu and
//! keeps it in sync with the app once per frame: item labels follow the UI
//! language, items are enabled as [`can_execute`](VSignalNodesApp::can_execute)
//! says.
//!
//! Clicks and key equivalents arrive as `muda` events, drained by
//! [`NativeMenu::take_commands`] from eframe's `raw_input_hook` before the frame's
//! input is processed. Copy and Paste are turned into the same egui events as
//! their keys would be (see `ui::menu`), so a focused text field gets them too.
//!
//! Only shortcuts with a modifier become native key equivalents: AppKit gives
//! those to the menu before the window sees them, so egui never does and nothing
//! runs twice. Plain keys (Space) stay with egui, otherwise typing them into a
//! text field would run the command.

use eframe::egui;
use muda::accelerator::{Key, KeyAccelerator, Modifiers, NamedKey};
use muda::{MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

use super::commands::{Command, Menu};
use super::{Language, VSignalNodesApp};

/// The native menu bar and the commands of its items.
pub(super) struct NativeMenu {
    /// Kept alive: dropping the menu removes it from the menu bar.
    _bar: muda::Menu,
    /// The editor's menus, in [`Menu::ALL`] order.
    menus: Vec<(Menu, Submenu)>,
    /// Every command item, in menu order.
    items: Vec<(Command, MenuItem)>,
    /// The language the labels are in.
    language: Language,
}

impl NativeMenu {
    /// Builds the menu bar in `language`, installs it as the application menu
    /// and makes every menu event wake `ctx` (otherwise an idle app would only
    /// see the click on its next repaint).
    pub(super) fn install(ctx: &egui::Context, language: Language) -> muda::Result<Self> {
        let bar = muda::Menu::new();
        // The first submenu is the application menu; macOS titles it with the app name.
        bar.append(&Submenu::with_items(
            "",
            true,
            &[
                &PredefinedMenuItem::about(None, None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ],
        )?)?;
        let mut menus = Vec::new();
        let mut items = Vec::new();
        for menu in Menu::ALL {
            let submenu = Submenu::new(language.text(menu.text()), true);
            for &command in menu.commands() {
                let item = MenuItem::new(language.text(command.text()), true, None);
                item.set_key_accelerator(key_accelerator(command))?;
                submenu.append(&item)?;
                items.push((command, item));
            }
            bar.append(&submenu)?;
            menus.push((menu, submenu));
        }
        bar.init_for_nsapp();

        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |_| ctx.request_repaint()));
        Ok(Self {
            _bar: bar,
            menus,
            items,
            language,
        })
    }

    /// Brings labels and enabled states up to date with `app`. Only items that
    /// changed are touched, so calling it every frame is cheap.
    pub(super) fn sync(&mut self, app: &VSignalNodesApp) {
        if self.language != app.language {
            self.language = app.language;
            for (menu, submenu) in &self.menus {
                submenu.set_text(self.language.text(menu.text()));
            }
            for (command, item) in &self.items {
                item.set_text(self.language.text(command.text()));
            }
        }
        for (command, item) in &self.items {
            let enabled = match command {
                // A focused text field copies and pastes even when the graph can't.
                Command::Copy | Command::Paste => true,
                _ => app.can_execute(*command),
            };
            if item.is_enabled() != enabled {
                item.set_enabled(enabled);
            }
        }
    }

    /// The commands picked in the menu since the last call, in order.
    pub(super) fn take_commands(&self) -> Vec<Command> {
        let mut commands = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some((command, _)) = self.items.iter().find(|(_, item)| *item.id() == event.id) {
                commands.push(*command);
            }
        }
        commands
    }
}

/// The native key equivalent of `command`: its first shortcut that has a
/// modifier and a key `muda` can name, Cmd for [`egui::Modifiers::COMMAND`].
fn key_accelerator(command: Command) -> Option<KeyAccelerator> {
    command.shortcuts().iter().find_map(|shortcut| {
        let egui_modifiers = shortcut.modifiers;
        if !(egui_modifiers.command || egui_modifiers.alt || egui_modifiers.ctrl) {
            return None;
        }
        let mut modifiers = Modifiers::empty();
        if egui_modifiers.command || egui_modifiers.mac_cmd {
            modifiers |= Modifiers::META;
        }
        if egui_modifiers.ctrl && !egui_modifiers.command {
            modifiers |= Modifiers::CONTROL;
        }
        if egui_modifiers.alt {
            modifiers |= Modifiers::ALT;
        }
        if egui_modifiers.shift {
            modifiers |= Modifiers::SHIFT;
        }
        // Not `egui::Key::symbol_or_name`: its minus is U+2212, not the key's `-`.
        let key = match shortcut.logical_key {
            egui::Key::Enter => Key::Named(NamedKey::Enter),
            egui::Key::Plus => Key::Character("+".into()),
            egui::Key::Equals => Key::Character("=".into()),
            egui::Key::Minus => Key::Character("-".into()),
            egui::Key::Num0 => Key::Character("0".into()),
            egui::Key::C => Key::Character("c".into()),
            egui::Key::V => Key::Character("v".into()),
            // Add keys here as commands start using them.
            _ => return None,
        };
        Some(KeyAccelerator::new(modifiers, key))
    })
}

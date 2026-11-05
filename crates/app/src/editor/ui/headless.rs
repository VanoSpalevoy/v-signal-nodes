//! Running UI frames without a window, with simulated pointer events, for tests.

use eframe::egui;

/// The simulated screen.
pub(super) const SCREEN: egui::Rect = egui::Rect {
    min: egui::Pos2::ZERO,
    max: egui::pos2(1000.0, 800.0),
};

/// A context and its clock.
pub(super) struct Frames {
    pub(super) ctx: egui::Context,
    time: f64,
}

impl Frames {
    pub(super) fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            time: 0.0,
        }
    }

    /// Runs one frame of `body` with `events`; returns what it painted.
    pub(super) fn run(
        &mut self,
        events: Vec<egui::Event>,
        body: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        self.time += 0.02;
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let mut output = self.ctx.run_ui(input, body);
        // No renderer to take the textures.
        output.textures_delta.clear();
        output
    }
}

/// The primary button going down or up at `pos`.
pub(super) fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// The events of the frames that move the pointer to `pos` and click there.
pub(super) fn click(pos: egui::Pos2) -> [Vec<egui::Event>; 4] {
    [
        vec![egui::Event::PointerMoved(pos)],
        vec![button(pos, true)],
        vec![button(pos, false)],
        Vec::new(),
    ]
}

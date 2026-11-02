//! A plot window's contents: the newest frame a node published, on a graticule.
//!
//! Scales follow the frame: the horizontal axis is its whole length, the vertical one
//! its range rounded out to steps of 1, 2 or 5 times a power of ten, so the scale holds
//! still while the signal changes a little. Values are labeled with their unit and an
//! SI prefix (`2 ms`, `500 мВ`) in the app's language.

use eframe::egui;
use v_signal_nodes_core::AxisUnit;
use v_signal_nodes_runtime::plots::PlotFrame;

use super::{ACCENT, BORDER, TEXT_DIM, WINDOW_BG};
use crate::editor::{Language, Text};

/// Rough number of divisions per axis; the 1-2-5 steps decide the exact one.
const DIVISIONS: f32 = 8.0;
/// Room for the vertical axis's labels, on the left of the screen.
const LEFT_MARGIN: f32 = 56.0;
/// Room for the horizontal axis's labels, below the screen.
const BOTTOM_MARGIN: f32 = 20.0;
/// Free height between the summary line and the screen; the top label of the vertical
/// axis, centered on the screen's edge, reaches into it.
const SUMMARY_GAP: f32 = 8.0;
/// Smallest size of the screen.
const MIN_SIZE: egui::Vec2 = egui::vec2(160.0, 100.0);
/// Range of the vertical axis when the signal is constant, around its value.
const FLAT_RANGE: f32 = 1.0;
/// Signals that vary less than this count as constant. Rounding leaves about 100 nV of
/// noise where voltages cancel out, and that shouldn't fill the screen as a signal.
const MIN_VARIATION: f32 = 1e-6;
/// Size of the labels.
const LABEL_SIZE: f32 = 11.0;

/// Shows `frame`, if any, with axes in units `x` and `y`, filling `ui`.
pub(super) fn show(
    ui: &mut egui::Ui,
    frame: Option<&PlotFrame>,
    x: AxisUnit,
    y: AxisUnit,
    language: Language,
) {
    let measured = frame.and_then(Measured::of);
    let summary = match &measured {
        Some(measured) => format!(
            "{} {}   {} {}   {} {}",
            language.text(Text::PlotMin),
            quantity(measured.low, y, language),
            language.text(Text::PlotMax),
            quantity(measured.high, y, language),
            language.text(Text::PlotPeakToPeak),
            quantity(measured.high - measured.low, y, language),
        ),
        None => language.text(Text::PlotNoData).to_owned(),
    };

    let (rect, _) = ui.allocate_exact_size(available(ui), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    // Laid out first: the screen starts below however many lines it wraps to.
    let summary = painter.layout(
        summary.to_uppercase(),
        egui::FontId::proportional(LABEL_SIZE),
        TEXT_DIM,
        rect.width(),
    );
    let screen = egui::Rect::from_min_max(
        rect.min + egui::vec2(LEFT_MARGIN, summary.size().y + SUMMARY_GAP),
        rect.max - egui::vec2(0.0, BOTTOM_MARGIN),
    );
    painter.galley(rect.min, summary, TEXT_DIM);
    painter.rect_filled(screen, 4.0, WINDOW_BG);
    if let Some(measured) = measured {
        draw(&painter, screen, &measured, x, y, language);
    }
}

/// The size left in `ui`, at least [`MIN_SIZE`] plus the margins and a summary line.
fn available(ui: &egui::Ui) -> egui::Vec2 {
    let margins = egui::vec2(LEFT_MARGIN, LABEL_SIZE * 2.0 + SUMMARY_GAP + BOTTOM_MARGIN);
    ui.available_size().max(MIN_SIZE + margins)
}

/// A frame with what the axes need to know about it.
struct Measured<'a> {
    samples: &'a [f32],
    x_step: f32,
    /// Lowest and highest finite sample.
    low: f32,
    high: f32,
}

impl<'a> Measured<'a> {
    /// `None` if there's nothing to draw: a frame without a finite sample.
    fn of(frame: &'a PlotFrame) -> Option<Self> {
        let finite = || {
            frame
                .samples
                .iter()
                .copied()
                .filter(|sample| sample.is_finite())
        };
        let low = finite().reduce(f32::min)?;
        let high = finite().reduce(f32::max)?;
        Some(Self {
            samples: &frame.samples,
            x_step: frame.x_step,
            low,
            high,
        })
    }

    /// The horizontal axis's length.
    fn duration(&self) -> f32 {
        self.x_step * self.samples.len().saturating_sub(1) as f32
    }
}

/// Draws the graticule on `screen`, its labels and the trace.
fn draw(
    painter: &egui::Painter,
    screen: egui::Rect,
    frame: &Measured,
    x: AxisUnit,
    y: AxisUnit,
    language: Language,
) {
    let (bottom, top, step) = vertical_scale(frame.low, frame.high);
    let duration = frame.duration();
    let to_x =
        |value: f32| screen.left() + screen.width() * value / duration.max(f32::MIN_POSITIVE);
    let to_y = |value: f32| screen.bottom() - screen.height() * (value - bottom) / (top - bottom);
    let grid = egui::Stroke::new(1.0, BORDER);
    let font = egui::FontId::proportional(LABEL_SIZE);

    for value in ticks(bottom, top, step) {
        painter.hline(screen.x_range(), to_y(value), grid);
        painter.text(
            egui::pos2(screen.left() - 6.0, to_y(value)),
            egui::Align2::RIGHT_CENTER,
            quantity(value, y, language),
            font.clone(),
            TEXT_DIM,
        );
    }
    let x_step = nice_step(duration / DIVISIONS);
    for value in ticks(0.0, duration, x_step) {
        painter.vline(to_x(value), screen.y_range(), grid);
        painter.text(
            egui::pos2(to_x(value), screen.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            quantity(value, x, language),
            font.clone(),
            TEXT_DIM,
        );
    }

    let points: Vec<egui::Pos2> = frame
        .samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| sample.is_finite())
        .map(|(index, &sample)| egui::pos2(to_x(index as f32 * frame.x_step), to_y(sample)))
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, ACCENT)));
}

/// The vertical axis for samples within `low..=high`: bottom, top and the step
/// between grid lines. A constant signal gets [`FLAT_RANGE`] around it.
fn vertical_scale(low: f32, high: f32) -> (f32, f32, f32) {
    let (low, high) = if high - low > MIN_VARIATION {
        (low, high)
    } else {
        let middle = (low + high) * 0.5;
        (middle - FLAT_RANGE, middle + FLAT_RANGE)
    };
    let step = nice_step((high - low) / DIVISIONS);
    let bottom = (low / step).floor() * step;
    let top = (high / step).ceil() * step;
    (bottom, top, step)
}

/// The value of the form 1, 2 or 5 times a power of ten nearest above `rough`.
fn nice_step(rough: f32) -> f32 {
    if !(rough > 0.0 && rough.is_finite()) {
        return 1.0;
    }
    let power = 10.0_f32.powf(rough.log10().floor());
    let mantissa = rough / power;
    let nice = if mantissa <= 1.0 {
        1.0
    } else if mantissa <= 2.0 {
        2.0
    } else if mantissa <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * power
}

/// The multiples of `step` within `from..=to`.
fn ticks(from: f32, to: f32, step: f32) -> impl Iterator<Item = f32> {
    let first = (from / step).ceil() as i64;
    let last = ((to + step * 1e-3) / step).floor() as i64;
    (first..=last).map(move |index| index as f32 * step)
}

/// `value` with its unit and an SI prefix, in `language`: "2 ms", "-500 мВ".
fn quantity(value: f32, unit: AxisUnit, language: Language) -> String {
    let unit = language.text(match unit {
        AxisUnit::Seconds => Text::UnitSeconds,
        AxisUnit::Volts => Text::UnitVolts,
    });
    let size = value.abs();
    let (scale, prefix) = if size == 0.0 {
        (1.0, None)
    } else if size < 1e-6 {
        (1e9, Some(Text::PrefixNano))
    } else if size < 1e-3 {
        (1e6, Some(Text::PrefixMicro))
    } else if size < 1.0 {
        (1e3, Some(Text::PrefixMilli))
    } else if size < 1e3 {
        (1.0, None)
    } else if size < 1e6 {
        (1e-3, Some(Text::PrefixKilo))
    } else {
        (1e-6, Some(Text::PrefixMega))
    };
    let prefix = prefix.map_or("", |prefix| language.text(prefix));
    // Single precision has about seven digits: rounding hides the noise of the steps.
    let scaled = (f64::from(value) * scale * 1000.0).round() / 1000.0;
    format!("{scaled} {prefix}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_stays_above_the_screen_also_when_it_wraps() {
        let frame = PlotFrame {
            x_step: 1e-5,
            samples: (0..500).map(|i| (i as f32 * 0.05).sin()).collect(),
        };
        let min = Language::Russian.text(Text::PlotMin).to_uppercase();
        for width in [480.0, 320.0, 240.0, 120.0] {
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 300.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                show(
                    ui,
                    Some(&frame),
                    AxisUnit::Seconds,
                    AxisUnit::Volts,
                    Language::Russian,
                );
            });
            // No renderer to take the textures.
            output.textures_delta.clear();

            let mut summary = None;
            let mut screen = None;
            for clipped in &output.shapes {
                match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.text().starts_with(&min) => {
                        summary = Some(text.visual_bounding_rect());
                    }
                    egui::Shape::Rect(rect) if rect.fill == WINDOW_BG => screen = Some(rect.rect),
                    _ => {}
                }
            }
            let (summary, screen) = (summary.unwrap(), screen.unwrap());
            assert!(
                summary.bottom() < screen.top(),
                "at width {width}: {summary:?} over {screen:?}"
            );
        }
    }

    #[test]
    fn steps_are_one_two_or_five_times_a_power_of_ten() {
        assert_eq!(nice_step(0.9), 1.0);
        assert_eq!(nice_step(1.5), 2.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert!((nice_step(0.0013) - 0.002).abs() < 1e-7);
        assert_eq!(nice_step(0.0), 1.0, "an empty axis still has a step");
    }

    #[test]
    fn the_vertical_scale_covers_the_signal_on_whole_steps() {
        let (bottom, top, step) = vertical_scale(-0.98, 1.02);
        assert_eq!(step, 0.5);
        assert_eq!((bottom, top), (-1.0, 1.5));

        let (bottom, top, _) = vertical_scale(5.0, 5.0);
        assert!(
            bottom < 5.0 && top > 5.0,
            "a constant voltage isn't flat at an edge"
        );

        let (bottom, top, _) = vertical_scale(-6e-8, 6e-8);
        assert_eq!((bottom, top), (-1.0, 1.0), "rounding noise isn't a signal");
        let (bottom, top, _) = vertical_scale(-0.001, 0.001);
        assert!(top - bottom < 0.01, "a small signal still fills the screen");
    }

    #[test]
    fn ticks_reach_both_ends() {
        let ticks: Vec<_> = ticks(-1.0, 1.0, 0.5).collect();
        assert_eq!(ticks, [-1.0, -0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn quantities_have_their_unit_and_prefix_in_the_language() {
        use AxisUnit::{Seconds, Volts};
        let english = Language::English;
        let russian = Language::Russian;

        assert_eq!(quantity(0.002, Seconds, english), "2 ms");
        assert_eq!(quantity(0.002, Seconds, russian), "2 мс");
        assert_eq!(quantity(-0.5, Volts, english), "-500 mV");
        assert_eq!(quantity(-0.5, Volts, russian), "-500 мВ");
        assert_eq!(quantity(12.5, Volts, english), "12.5 V");
        assert_eq!(quantity(0.0, Volts, english), "0 V");
        assert_eq!(quantity(1500.0, Volts, english), "1.5 kV");
        assert_eq!(quantity(0.000_25, Seconds, russian), "250 мкс");
    }

    #[test]
    fn any_frame_can_be_drawn() {
        use crate::editor::ui::headless::Frames;
        let sine: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin()).collect();
        let frames = [
            None,
            Some(PlotFrame {
                x_step: 1e-5,
                samples: sine,
            }),
            Some(PlotFrame {
                x_step: 1.0,
                samples: vec![2.5],
            }),
            Some(PlotFrame {
                x_step: 1e-6,
                samples: vec![0.0, f32::NAN, 1e6],
            }),
            Some(PlotFrame::default()),
        ];
        let mut headless = Frames::new();
        for frame in &frames {
            headless.run(Vec::new(), |ui| {
                show(
                    ui,
                    frame.as_ref(),
                    AxisUnit::Seconds,
                    AxisUnit::Volts,
                    Language::Russian,
                );
            });
        }
    }

    #[test]
    fn a_frame_without_finite_samples_has_nothing_to_draw() {
        let frame = PlotFrame {
            x_step: 1.0,
            samples: vec![f32::NAN, f32::INFINITY],
        };
        assert!(Measured::of(&frame).is_none());

        let frame = PlotFrame {
            x_step: 0.5,
            samples: vec![1.0, f32::NAN, -1.0],
        };
        let measured = Measured::of(&frame).unwrap();
        assert_eq!((measured.low, measured.high), (-1.0, 1.0));
        assert_eq!(measured.duration(), 1.0);
    }
}

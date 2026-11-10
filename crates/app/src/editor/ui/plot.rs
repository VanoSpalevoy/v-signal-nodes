//! A plot window's contents: the newest frame a node published, on a graticule.
//!
//! Scales follow the frame: the horizontal axis is its whole length, the vertical one
//! its range rounded out to steps of 1, 2 or 5 times a power of ten, so the scale holds
//! still while the signal changes a little. Values are labeled with their unit and an
//! SI prefix (`2 ms`, `500 мВ`) in the app's language.
//!
//! A frame is drawn as its [`PlotTrace`] says: straight lines between the points, or,
//! for samples of a band-limited signal, the signal they stand for, interpolated by
//! sin(x)/x (a Lanczos-windowed sinc) once per pixel, as digital oscilloscopes do. Its
//! peaks may lie between the samples, so the scale fits the trace, while the min, max
//! and peak-to-peak values are measured on the samples.
//!
//! A frame of several beams (a multi-beam oscilloscope) has them drawn over each other
//! on the same axes, each in its color ([`BEAM_COLORS`]), the first on top, with the
//! summary of each in its color after its number. A marker with the same number in
//! the beam's color points at the end of each trace from the right of the screen, as
//! channel markers do on oscilloscopes, so beams are told apart without the colors
//! too.

use eframe::egui;
use v_signal_nodes_core::{MAX_PLOT_BEAMS, Unit};
use v_signal_nodes_runtime::plots::{PlotFrame, PlotTrace};

use super::{ACCENT, BORDER, GENERATOR_GREEN, TEXT_DIM, WINDOW_BG};
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
/// Samples on each side of a point of an interpolated trace that it's computed from:
/// the `a` of the Lanczos window. Plenty for a sine of 2.5 samples per period.
const SINC_TAPS: usize = 8;
/// Interpolate only when samples are at least this far apart on the screen: closer
/// ones already show the signal, and lines between them look the same.
const MIN_INTERPOLATED_SPACING: f32 = 2.0;

/// Size of a beam's marker: its number on a square of its color.
const MARKER_SIZE: f32 = 14.0;
/// Room for the beams' markers, on the right of the screen of a multi-beam frame.
const RIGHT_MARGIN: f32 = MARKER_SIZE + 6.0;

/// Colors of the beams of a frame, in order: the first in the accent color, the second
/// in the generators' green, the others in colors that stand apart from them and from
/// each other on the dark screen.
const BEAM_COLORS: [egui::Color32; MAX_PLOT_BEAMS] = [
    ACCENT,
    GENERATOR_GREEN,
    egui::Color32::from_rgb(224, 112, 200),
    egui::Color32::from_rgb(240, 196, 84),
];

/// Shows `frame`, if any, with axes in units `x` and `y`, filling `ui`. Every beam of
/// the frame is drawn on the same axes in its own color ([`BEAM_COLORS`]).
pub(super) fn show(
    ui: &mut egui::Ui,
    frame: Option<&PlotFrame>,
    x: Unit,
    y: Unit,
    language: Language,
) {
    let beams = frame.map(beams_of).unwrap_or_default();
    // One beam reads as before; several are told apart by number and color.
    let numbered = frame.is_some_and(|frame| frame.beams > 1);
    let font = egui::FontId::proportional(LABEL_SIZE);
    let mut summary = egui::text::LayoutJob::default();
    if beams.is_empty() {
        summary.append(
            &language.text(Text::PlotNoData).to_uppercase(),
            0.0,
            egui::TextFormat::simple(font.clone(), TEXT_DIM),
        );
    }
    for (index, (beam, measured)) in beams.iter().enumerate() {
        let (label, color) = if numbered {
            (format!("{}: ", beam + 1), BEAM_COLORS[*beam])
        } else {
            (String::new(), TEXT_DIM)
        };
        let text = format!(
            "{}{label}{} {}   {} {}   {} {}",
            if index > 0 { "      " } else { "" },
            language.text(Text::PlotMin),
            quantity(measured.low, y, language),
            language.text(Text::PlotMax),
            quantity(measured.high, y, language),
            language.text(Text::PlotPeakToPeak),
            quantity(measured.high - measured.low, y, language),
        );
        summary.append(
            &text.to_uppercase(),
            0.0,
            egui::TextFormat::simple(font.clone(), color),
        );
    }

    let (rect, _) = ui.allocate_exact_size(available(ui), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    // Laid out first: the screen starts below however many lines it wraps to.
    summary.wrap.max_width = rect.width();
    let summary = painter.layout_job(summary);
    let right = if numbered { RIGHT_MARGIN } else { 0.0 };
    let screen = egui::Rect::from_min_max(
        rect.min + egui::vec2(LEFT_MARGIN, summary.size().y + SUMMARY_GAP),
        rect.max - egui::vec2(right, BOTTOM_MARGIN),
    );
    painter.galley(rect.min, summary, TEXT_DIM);
    painter.rect_filled(screen, 4.0, WINDOW_BG);
    if !beams.is_empty() {
        let ends = draw(&painter, screen, &beams, x, y, language);
        if numbered {
            draw_markers(&painter, screen, ends);
        }
    }
}

/// Draws the marker of every beam right of `screen`: its number on a square of its
/// color, pointing at `ends`, the height of the end of its trace. Markers that would
/// overlap are moved apart, all within the screen's height.
fn draw_markers(painter: &egui::Painter, screen: egui::Rect, ends: Vec<(usize, f32)>) {
    let font = egui::FontId::proportional(LABEL_SIZE);
    for (beam, y) in marker_heights(ends, screen.y_range()) {
        let marker = egui::Rect::from_center_size(
            egui::pos2(screen.right() + 4.0 + MARKER_SIZE * 0.5, y),
            egui::vec2(MARKER_SIZE, MARKER_SIZE),
        );
        let color = BEAM_COLORS[beam];
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(screen.right(), y),
                marker.left_top(),
                marker.left_bottom(),
            ],
            color,
            egui::Stroke::NONE,
        ));
        painter.rect_filled(marker, 2.0, color);
        painter.text(
            marker.center(),
            egui::Align2::CENTER_CENTER,
            (beam + 1).to_string(),
            font.clone(),
            WINDOW_BG,
        );
    }
}

/// Centers of the markers of beams whose traces end at heights `ends`, in order from
/// the top: as close to their ends as they can be at least [`MARKER_SIZE`] apart and
/// whole within `range` (if it has room for them all).
fn marker_heights(mut ends: Vec<(usize, f32)>, range: egui::Rangef) -> Vec<(usize, f32)> {
    let half = MARKER_SIZE * 0.5;
    let (top, bottom) = (range.min + half, range.max - half);
    ends.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    // Down from the top, then back up from the bottom.
    let mut below = top - MARKER_SIZE;
    for (_, y) in &mut ends {
        *y = y.max(below + MARKER_SIZE);
        below = *y;
    }
    let mut above = bottom + MARKER_SIZE;
    for (_, y) in ends.iter_mut().rev() {
        *y = y.min(above - MARKER_SIZE);
        above = *y;
    }
    ends
}

/// The beams of `frame` that have something to draw, with their index.
fn beams_of(frame: &PlotFrame) -> Vec<(usize, Measured<'_>)> {
    frame
        .beams()
        .take(MAX_PLOT_BEAMS)
        .enumerate()
        .filter_map(|(index, samples)| {
            Some((index, Measured::of(samples, frame.x_step, frame.trace)?))
        })
        .collect()
}

/// The size left in `ui`, at least [`MIN_SIZE`] plus the margins and a summary line.
fn available(ui: &egui::Ui) -> egui::Vec2 {
    let margins = egui::vec2(LEFT_MARGIN, LABEL_SIZE * 2.0 + SUMMARY_GAP + BOTTOM_MARGIN);
    ui.available_size().max(MIN_SIZE + margins)
}

/// One beam of a frame with what the axes need to know about it.
struct Measured<'a> {
    samples: &'a [f32],
    x_step: f32,
    trace: PlotTrace,
    /// Lowest and highest finite sample.
    low: f32,
    high: f32,
}

impl<'a> Measured<'a> {
    /// A beam of `samples`, `x_step` apart, drawn as `trace`; `None` if there's
    /// nothing to draw: no finite sample.
    fn of(samples: &'a [f32], x_step: f32, trace: PlotTrace) -> Option<Self> {
        let finite = || samples.iter().copied().filter(|sample| sample.is_finite());
        let low = finite().reduce(f32::min)?;
        let high = finite().reduce(f32::max)?;
        Some(Self {
            samples,
            x_step,
            trace,
            low,
            high,
        })
    }

    /// The horizontal axis's length.
    fn duration(&self) -> f32 {
        self.x_step * self.samples.len().saturating_sub(1) as f32
    }

    /// The points of the trace on a screen `width` wide, as (sample index, value):
    /// one per pixel if interpolated, else the finite samples. Interpolated only if
    /// the frame asks for it, its samples are all finite and far enough apart.
    fn trace(&self, width: f32) -> Vec<(f32, f32)> {
        let last = self.samples.len().saturating_sub(1);
        let spacing = width / last.max(1) as f32;
        let interpolated = self.trace == PlotTrace::Interpolated
            && last > 0
            && spacing >= MIN_INTERPOLATED_SPACING
            && self.samples.iter().all(|sample| sample.is_finite());
        if !interpolated {
            return self
                .samples
                .iter()
                .enumerate()
                .filter(|(_, sample)| sample.is_finite())
                .map(|(index, &sample)| (index as f32, sample))
                .collect();
        }
        let points = width.ceil() as usize;
        (0..=points)
            .map(|point| {
                let at = point as f32 * last as f32 / points as f32;
                (at, interpolate(self.samples, at))
            })
            .collect()
    }
}

/// The band-limited signal of `samples` at sample index `at` (may be fractional):
/// the sum of the [`SINC_TAPS`] samples on each side weighted by the Lanczos kernel,
/// normalized by the weights, so it doesn't sag near the ends where samples are
/// missing. Exactly the sample at a whole index.
fn interpolate(samples: &[f32], at: f32) -> f32 {
    let taps = SINC_TAPS as f32;
    let lanczos = |x: f32| {
        if x == 0.0 {
            return 1.0;
        }
        let pi_x = std::f32::consts::PI * x;
        taps * pi_x.sin() * (pi_x / taps).sin() / (pi_x * pi_x)
    };
    let nearest = at.floor() as usize;
    let first = (nearest + 1).saturating_sub(SINC_TAPS);
    let end = (nearest + SINC_TAPS + 1).min(samples.len());
    let (sum, weights) = (first..end).fold((0.0, 0.0), |(sum, weights), index| {
        let weight = lanczos(at - index as f32);
        (sum + weight * samples[index], weights + weight)
    });
    sum / weights
}

/// Draws the graticule on `screen`, its labels and the trace of every beam (at least
/// one), on axes that fit them all. Returns the height on the screen where each beam's
/// trace ends.
fn draw(
    painter: &egui::Painter,
    screen: egui::Rect,
    beams: &[(usize, Measured)],
    x: Unit,
    y: Unit,
    language: Language,
) -> Vec<(usize, f32)> {
    let traces: Vec<_> = beams
        .iter()
        .map(|(_, beam)| beam.trace(screen.width()))
        .collect();
    // The traces' own range: an interpolated one may reach past the samples.
    let (low, high) = beams
        .iter()
        .map(|(_, beam)| (beam.low, beam.high))
        .chain(traces.iter().flatten().map(|&(_, value)| (value, value)))
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), (l, h)| {
            (low.min(l), high.max(h))
        });
    let (bottom, top, step) = vertical_scale(low, high);
    let x_step = beams[0].1.x_step;
    let duration = beams
        .iter()
        .map(|(_, beam)| beam.duration())
        .fold(0.0, f32::max);
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
    for value in ticks(0.0, duration, nice_step(duration / DIVISIONS)) {
        painter.vline(to_x(value), screen.y_range(), grid);
        painter.text(
            egui::pos2(to_x(value), screen.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            quantity(value, x, language),
            font.clone(),
            TEXT_DIM,
        );
    }

    let mut ends = Vec::with_capacity(beams.len());
    // In reverse, so the first beam is on top.
    for ((beam, _), trace) in beams.iter().zip(traces).rev() {
        let points: Vec<egui::Pos2> = trace
            .into_iter()
            .map(|(index, value)| egui::pos2(to_x(index * x_step), to_y(value)))
            .collect();
        if let Some(end) = points.last() {
            ends.push((*beam, end.y));
        }
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5, BEAM_COLORS[*beam]),
        ));
    }
    ends
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

/// `value` with its unit and an SI prefix, in `language`: "2 ms", "-500 мВ". Degrees
/// get no prefix.
pub(super) fn quantity(value: f32, unit: Unit, language: Language) -> String {
    let symbol = language.text(match unit {
        Unit::Seconds => Text::UnitSeconds,
        Unit::Volts => Text::UnitVolts,
        Unit::Hertz => Text::UnitHertz,
        Unit::Degrees => Text::UnitDegrees,
    });
    let size = value.abs();
    let (scale, prefix) = if size == 0.0 || unit == Unit::Degrees {
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
    if unit == Unit::Degrees {
        // Written next to the number: 90°.
        return format!("{scaled}{symbol}");
    }
    format!("{scaled} {prefix}{symbol}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_stays_above_the_screen_also_when_it_wraps() {
        let frame = PlotFrame {
            x_step: 1e-5,
            beams: 1,
            samples: (0..500).map(|i| (i as f32 * 0.05).sin()).collect(),
            trace: PlotTrace::Lines,
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
                    Unit::Seconds,
                    Unit::Volts,
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
        use Unit::{Seconds, Volts};
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
        assert_eq!(quantity(44_100.0, Unit::Hertz, english), "44.1 kHz");
        assert_eq!(quantity(2e6, Unit::Hertz, russian), "2 МГц");
        assert_eq!(quantity(1500.0, Unit::Degrees, english), "1500°");
    }

    #[test]
    fn any_frame_can_be_drawn() {
        use crate::editor::ui::headless::Frames;
        let sine: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin()).collect();
        let few: Vec<f32> = (0..12).map(|i| (i as f32 * 1.3).sin()).collect();
        let samples = [
            sine,
            few,
            vec![2.5],
            vec![0.0, f32::NAN, 1e6],
            vec![0.0, 1e6],
            Vec::new(),
        ];
        let mut headless = Frames::new();
        headless.run(Vec::new(), |ui| {
            show(ui, None, Unit::Seconds, Unit::Volts, Language::Russian);
        });
        for samples in samples {
            for trace in [PlotTrace::Lines, PlotTrace::Interpolated] {
                let frame = PlotFrame {
                    x_step: 1e-5,
                    beams: 1,
                    samples: samples.clone(),
                    trace,
                };
                headless.run(Vec::new(), |ui| {
                    show(
                        ui,
                        Some(&frame),
                        Unit::Seconds,
                        Unit::Volts,
                        Language::Russian,
                    );
                });
            }
        }
    }

    #[test]
    fn interpolation_brings_back_a_sine_of_few_samples_per_period() {
        // 10 kHz at 48 kHz: 4.8 samples per period, a triangle when joined by lines.
        let samples_per_period = 4.8;
        let sine = |at: f32| (std::f32::consts::TAU * at / samples_per_period).sin();
        let samples: Vec<f32> = (0..200).map(|index| sine(index as f32)).collect();

        // Halfway between samples, away from the ends of the frame.
        for index in 20..180 {
            let at = index as f32 + 0.5;
            let error = (interpolate(&samples, at) - sine(at)).abs();
            assert!(error < 0.01, "at {at}: off by {error}");
        }
        assert_eq!(interpolate(&samples, 7.0), samples[7], "exact at a sample");
    }

    #[test]
    fn only_samples_far_enough_apart_are_interpolated_and_only_when_asked() {
        let frame = |trace| PlotFrame {
            x_step: 1.0,
            beams: 1,
            samples: (0..50).map(|i| (i as f32).sin()).collect(),
            trace,
        };
        let lines = frame(PlotTrace::Lines);
        let interpolated = frame(PlotTrace::Interpolated);
        let points = |frame: &PlotFrame, width| {
            Measured::of(&frame.samples, frame.x_step, frame.trace)
                .unwrap()
                .trace(width)
                .len()
        };

        assert_eq!(points(&lines, 1000.0), 50);
        assert_eq!(points(&interpolated, 1000.0), 1001, "a point per pixel");
        assert_eq!(points(&interpolated, 60.0), 50, "dense enough already");

        let mut with_gap = frame(PlotTrace::Interpolated);
        with_gap.samples[10] = f32::NAN;
        assert_eq!(
            points(&with_gap, 1000.0),
            49,
            "lines around a missing sample"
        );
    }

    #[test]
    fn a_frame_without_finite_samples_has_nothing_to_draw() {
        let frame = PlotFrame {
            x_step: 1.0,
            beams: 1,
            samples: vec![f32::NAN, f32::INFINITY],
            trace: PlotTrace::Lines,
        };
        assert!(Measured::of(&frame.samples, frame.x_step, frame.trace).is_none());

        let frame = PlotFrame {
            x_step: 0.5,
            beams: 1,
            samples: vec![1.0, f32::NAN, -1.0],
            trace: PlotTrace::Lines,
        };
        let measured = Measured::of(&frame.samples, frame.x_step, frame.trace).unwrap();
        assert_eq!((measured.low, measured.high), (-1.0, 1.0));
        assert_eq!(measured.duration(), 1.0);
    }

    #[test]
    fn markers_stay_apart_and_on_the_screen() {
        let range = egui::Rangef::new(0.0, 100.0);
        assert_eq!(
            marker_heights(vec![(0, 70.0), (1, 30.0)], range),
            [(1, 30.0), (0, 70.0)],
            "far apart, each at its trace"
        );
        let heights = marker_heights(vec![(0, 50.0), (1, 50.0)], range);
        assert_eq!(heights[1].1 - heights[0].1, MARKER_SIZE, "{heights:?}");
        let heights = marker_heights(vec![(0, 100.0), (1, 99.0), (2, -5.0)], range);
        assert_eq!(
            heights,
            [
                (2, MARKER_SIZE * 0.5),
                (1, 100.0 - MARKER_SIZE * 1.5),
                (0, 100.0 - MARKER_SIZE * 0.5)
            ]
        );
    }

    #[test]
    fn every_beam_is_drawn_in_its_color_on_shared_axes() {
        let frame = PlotFrame {
            x_step: 1e-5,
            // A small sine and a large one; the second fills the scale.
            samples: (0..200)
                .map(|i| 0.1 * (i as f32 * 0.1).sin())
                .chain((0..200).map(|i| 5.0 * (i as f32 * 0.1).cos()))
                .collect(),
            beams: 2,
            trace: PlotTrace::Lines,
        };
        let beams = beams_of(&frame);
        assert_eq!(beams.len(), 2);
        assert!(beams[0].1.high < 0.2 && beams[1].1.high > 4.9);

        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 300.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            show(
                ui,
                Some(&frame),
                Unit::Seconds,
                Unit::Volts,
                Language::English,
            );
        });
        output.textures_delta.clear();
        let colors: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Path(path) if path.points.len() > 100 => {
                    Some(path.stroke.color.clone())
                }
                _ => None,
            })
            .collect();
        // The second first, so the first is on top.
        assert_eq!(
            colors,
            [
                egui::epaint::ColorMode::Solid(BEAM_COLORS[1]),
                egui::epaint::ColorMode::Solid(BEAM_COLORS[0])
            ]
        );

        // Each has its marker right of the screen, on its color.
        let screen = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) if rect.fill == WINDOW_BG => Some(rect.rect),
                _ => None,
            })
            .unwrap();
        let markers: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) if rect.rect.left() > screen.right() => Some(rect.fill),
                _ => None,
            })
            .collect();
        assert_eq!(markers, [BEAM_COLORS[1], BEAM_COLORS[0]]);

        // A beam without a finite sample isn't drawn; the other one still is.
        let mut broken = frame.clone();
        broken.samples[..200].fill(f32::NAN);
        let beams = beams_of(&broken);
        assert_eq!(beams.len(), 1);
        assert_eq!(beams[0].0, 1, "it keeps its number and color");
    }
}

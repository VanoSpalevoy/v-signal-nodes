//! Units of the numbers a node type declares: a plot's axes, a parameter's value.

/// What a number is measured in. An enum, not text, so an editor can translate the
/// unit, pick its prefix (ms, µV, kHz) and know what the number means: a parameter
/// in [`Hertz`](Self::Hertz) is a frequency, to compare with the sample rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    Seconds,
    Volts,
    Hertz,
    /// Angle in degrees, e.g. a phase.
    Degrees,
}

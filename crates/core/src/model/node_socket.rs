//! Node sockets: the named inputs and outputs of a node type, with the signal
//! each one carries.
//!
//! A socket's signal is described by what processing needs to know: its
//! [`SignalRate`] (how many values per block) and its number of channels. Together
//! they fix the size of the socket's buffer in a process call:
//! [`NodeSocket::buffer_len`].

/// Whether a socket receives a signal or produces one. Inputs order before
/// outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SocketDirection {
    Input,
    Output,
}

/// How often a signal has a new value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignalRate {
    /// A value per sample, at the host's sample rate: `frames` values per channel
    /// in every block. Audio and anything that must be sample-accurate.
    Audio,
    /// One value per block (per channel): modulation, levels, slowly changing
    /// controls. Cheap, but only as fine as the block size.
    Control,
}

/// Most channels a socket may have.
pub const MAX_CHANNELS: u16 = 64;

/// A named input or output of a node and the signal it carries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeSocket {
    pub name: String,
    pub direction: SocketDirection,
    pub rate: SignalRate,
    /// Number of channels, `1..=MAX_CHANNELS` (1 = mono, 2 = stereo).
    pub channels: u16,
}

impl NodeSocket {
    /// A mono input socket named `name` at `rate`.
    pub fn input(name: impl Into<String>, rate: SignalRate) -> Self {
        Self::new(name, SocketDirection::Input, rate)
    }

    /// A mono output socket named `name` at `rate`.
    pub fn output(name: impl Into<String>, rate: SignalRate) -> Self {
        Self::new(name, SocketDirection::Output, rate)
    }

    fn new(name: impl Into<String>, direction: SocketDirection, rate: SignalRate) -> Self {
        Self {
            name: name.into(),
            direction,
            rate,
            channels: 1,
        }
    }

    /// A mono audio-rate input.
    pub fn audio_in(name: impl Into<String>) -> Self {
        Self::input(name, SignalRate::Audio)
    }

    /// A mono audio-rate output.
    pub fn audio_out(name: impl Into<String>) -> Self {
        Self::output(name, SignalRate::Audio)
    }

    /// A mono control-rate input.
    pub fn control_in(name: impl Into<String>) -> Self {
        Self::input(name, SignalRate::Control)
    }

    /// A mono control-rate output.
    pub fn control_out(name: impl Into<String>) -> Self {
        Self::output(name, SignalRate::Control)
    }

    /// The same socket with `channels` channels.
    pub fn with_channels(self, channels: u16) -> Self {
        Self { channels, ..self }
    }

    /// Values per channel in a block of `frames`: `frames` at audio rate, one at
    /// control rate.
    pub fn channel_len(&self, frames: usize) -> usize {
        match self.rate {
            SignalRate::Audio => frames,
            SignalRate::Control => 1,
        }
    }

    /// Length of this socket's buffer in a block of `frames`: every channel's
    /// values, one channel after another (channel `c` at
    /// `c * channel_len..(c + 1) * channel_len`).
    pub fn buffer_len(&self, frames: usize) -> usize {
        self.channel_len(frames) * usize::from(self.channels)
    }

    /// Whether this output can feed `input`: the same rate, and the same number
    /// of channels or a mono side (mono into many channels is copied to each;
    /// many into mono are averaged).
    pub fn can_feed(&self, input: &NodeSocket) -> bool {
        self.rate == input.rate
            && (self.channels == input.channels || self.channels == 1 || input.channels == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_direction_rate_and_channels() {
        let input = NodeSocket::audio_in("in");
        let output = NodeSocket::control_out(String::from("out")).with_channels(2);

        assert_eq!(input.direction, SocketDirection::Input);
        assert_eq!(input.name, "in");
        assert_eq!(input.channels, 1);
        assert_eq!(output.direction, SocketDirection::Output);
        assert_eq!(output.rate, SignalRate::Control);
        assert_eq!(output.channels, 2);
    }

    #[test]
    fn buffer_sizes_follow_rate_and_channels() {
        let stereo = NodeSocket::audio_out("out").with_channels(2);
        assert_eq!(stereo.channel_len(64), 64);
        assert_eq!(stereo.buffer_len(64), 128);

        let control = NodeSocket::control_in("in").with_channels(3);
        assert_eq!(control.channel_len(64), 1);
        assert_eq!(control.buffer_len(64), 3);
    }

    #[test]
    fn mono_adapts_but_rates_must_match() {
        let mono = NodeSocket::audio_out("out");
        let stereo_in = NodeSocket::audio_in("in").with_channels(2);
        let stereo_out = NodeSocket::audio_out("out").with_channels(2);
        let quad_in = NodeSocket::audio_in("in").with_channels(4);

        assert!(mono.can_feed(&stereo_in));
        assert!(stereo_out.can_feed(&NodeSocket::audio_in("in")));
        assert!(stereo_out.can_feed(&stereo_in));
        assert!(!stereo_out.can_feed(&quad_in));
        assert!(!mono.can_feed(&NodeSocket::control_in("in")));
    }
}

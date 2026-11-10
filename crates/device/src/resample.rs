//! Rendering at one sample rate for a device running at another.
//!
//! The project's sample rate is chosen for the signals, not for the device, so the
//! graph may run at 192 kHz while the device takes 48 kHz. [`Resampled`] sits between
//! the [`Renderer`] and the device: it renders blocks at the renderer's rate and turns
//! them into the device's with a windowed-sinc resampler (`rubato`), on the device's
//! real-time thread, allocating nothing after it's made.
//!
//! Downsampling filters out what the device can't take (above half its rate), so a
//! graph at a high rate is heard without aliasing. The filter gets longer with the
//! ratio, up to [`MAX_SINC_LEN`]; past that its transition band widens.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use v_signal_nodes_runtime::realtime::Renderer;

use crate::DeviceError;

/// Frames of each resampled chunk: the device gets them a chunk at a time.
const CHUNK_FRAMES: usize = 256;
/// Length of the sinc filter at the device's rate or above, in input samples.
const SINC_LEN: usize = 256;
/// Longest sinc filter, for downsampling by large ratios: the filter of a ratio `r`
/// below 1 is `SINC_LEN / r` long, so its cost per device frame stays below this.
pub const MAX_SINC_LEN: usize = 2048;

/// A [`Renderer`] at its own rate, resampled to a device's rate.
pub struct Resampled {
    renderer: Renderer,
    resampler: Async<f32>,
    channels: usize,
    /// What the renderer rendered for the next chunk, interleaved.
    input: Vec<f32>,
    /// The last resampled chunk, interleaved: samples `sent..filled` are still to go.
    output: Vec<f32>,
    sent: usize,
    filled: usize,
}

impl Resampled {
    /// Resamples what `renderer` renders to `device_rate`, with `channels` interleaved
    /// channels per frame. Fails only if the rates are unusable.
    pub fn new(renderer: Renderer, device_rate: f32, channels: usize) -> Result<Self, DeviceError> {
        let channels = channels.max(1);
        let ratio = f64::from(device_rate) / f64::from(renderer.sample_rate());
        let sinc_len = ((SINC_LEN as f64 / ratio.min(1.0)).ceil() as usize).min(MAX_SINC_LEN);
        let parameters = SincInterpolationParameters {
            sinc_len,
            f_cutoff: None,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = Async::new_sinc(
            ratio,
            1.0,
            &parameters,
            CHUNK_FRAMES,
            channels,
            FixedAsync::Output,
        )
        .map_err(|err| DeviceError::Resampling(err.to_string()))?;
        Ok(Self {
            input: vec![0.0; resampler.input_frames_max() * channels],
            output: vec![0.0; resampler.output_frames_max() * channels],
            renderer,
            resampler,
            channels,
            sent: 0,
            filled: 0,
        })
    }

    /// Fills `device`, interleaved, with the next resampled frames, rendering as many
    /// blocks as that takes.
    pub fn render(&mut self, device: &mut [f32]) {
        let mut written = 0;
        while written < device.len() {
            if self.sent == self.filled {
                self.next_chunk();
            }
            let count = (self.filled - self.sent).min(device.len() - written);
            device[written..written + count]
                .copy_from_slice(&self.output[self.sent..self.sent + count]);
            self.sent += count;
            written += count;
        }
    }

    /// Renders and resamples the next chunk into `output`. Silence if resampling
    /// fails, which the buffer sizes rule out.
    fn next_chunk(&mut self) {
        let channels = self.channels;
        let frames_in = self.resampler.input_frames_next();
        let frames_out = self.resampler.output_frames_next();
        let input = &mut self.input[..frames_in * channels];
        self.renderer.render(input, channels);
        let resampled = InterleavedSlice::new(&*input, channels, frames_in)
            .ok()
            .zip(InterleavedSlice::new_mut(&mut self.output, channels, frames_out).ok())
            .and_then(|(input, mut output)| {
                self.resampler
                    .process_into_buffer(&input, &mut output, None)
                    .ok()
            });
        self.filled = match resampled {
            Some((_, frames)) if frames > 0 => frames * channels,
            _ => {
                self.output[..frames_out * channels].fill(0.0);
                frames_out * channels
            }
        };
        self.sent = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v_signal_nodes_core::{Graph, InputRef, OutputRef, ParamValue, compile};
    use v_signal_nodes_plugin_host::PluginHost;
    use v_signal_nodes_plugin_std::{DEVICE_OUTPUT, GENERATOR, StdPlugin};
    use v_signal_nodes_runtime::realtime;

    /// A generator of `frequency` at `rate` into an output node, resampled to
    /// `device_rate`: `frames` stereo frames of what the device gets.
    fn resampled_sine(frequency: f32, rate: f32, device_rate: f32, frames: usize) -> Vec<f32> {
        let mut host = PluginHost::new();
        host.load(&StdPlugin).unwrap();
        let mut graph = Graph::new();
        let source = graph.add(host.build_node(GENERATOR).unwrap());
        let sink = graph.add(host.build_node(DEVICE_OUTPUT).unwrap());
        graph
            .set_param(source, "frequency", ParamValue::Number(frequency))
            .unwrap();
        graph
            .connect(OutputRef::new(source, 0), InputRef::new(sink, 0))
            .unwrap();
        let (mut controller, renderer) = realtime::channel(rate, 64);
        controller
            .run_plan(compile(&graph).unwrap(), |step| {
                host.create_processor(&step.spec.type_id)
            })
            .unwrap();
        let mut resampled = Resampled::new(renderer, device_rate, 2).unwrap();
        let mut device = vec![9.0; frames * 2];
        // Odd pieces, so chunks end mid-block.
        for piece in device.chunks_mut(2 * 37) {
            resampled.render(piece);
        }
        device
    }

    /// Rising zero crossings of the left channel per second, after the filter's delay.
    fn frequency_of(device: &[f32], device_rate: f32) -> f32 {
        let left: Vec<f32> = device.iter().step_by(2).copied().skip(1024).collect();
        let crossings = left
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count();
        crossings as f32 * device_rate / left.len() as f32
    }

    #[test]
    fn a_higher_rate_reaches_the_device_at_its_own() {
        let device = resampled_sine(1_000.0, 192_000.0, 48_000.0, 48_000);

        let frequency = frequency_of(&device, 48_000.0);
        assert!((frequency - 1_000.0).abs() < 5.0, "{frequency}");
        let peak = device[2048..]
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.02, "{peak}");
        for [left, right] in device.as_chunks::<2>().0 {
            assert_eq!(left, right, "mono goes to both channels");
        }
    }

    #[test]
    fn a_lower_rate_reaches_the_device_at_its_own() {
        let device = resampled_sine(1_000.0, 44_100.0, 48_000.0, 48_000);

        let frequency = frequency_of(&device, 48_000.0);
        assert!((frequency - 1_000.0).abs() < 5.0, "{frequency}");
    }

    #[test]
    fn what_the_device_cant_take_is_filtered_out() {
        // 30 kHz at 192 kHz is above half of 48 kHz: without the filter it would come
        // back as an 18 kHz alias.
        let device = resampled_sine(30_000.0, 192_000.0, 48_000.0, 48_000);

        let peak = device[2048..]
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!(peak < 0.01, "{peak}");
    }
}

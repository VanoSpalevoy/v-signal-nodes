//! The input side: what a device takes in, for the graph's input nodes.
//!
//! The input device's callback runs on its own thread at its own rate, the renderer on
//! the output device's. Between them is a lock-free ring of the input's frames
//! (`rtrb`): the callback only converts and pushes. [`DeviceInput`], the renderer's
//! [`InputSource`], takes frames out on the output's real-time thread, resampled to
//! the renderer's rate if that's another one. Nothing allocates after starting.
//!
//! Two devices' clocks never quite agree, so the ring's fill drifts. Frames that
//! aren't there yet are silence (at the start, too), and once more than 150 ms pile
//! up the oldest are dropped down to 40 ms (`MAX_BACKLOG`, `BACKLOG`), so the delay
//! through the graph stays short.

use std::sync::mpsc::Sender;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::{Async, Resampler};
use v_signal_nodes_runtime::realtime::InputSource;

use crate::resample::{resample_chunk, sinc_resampler};
use crate::{DeviceError, Direction, find_device};

/// Seconds of input the ring holds; more is dropped in the callback.
const RING: f32 = 0.5;
/// Seconds of input waiting in the ring past which the oldest are dropped.
const MAX_BACKLOG: f32 = 0.15;
/// Seconds of input left waiting after dropping.
const BACKLOG: f32 = 0.04;

/// An input device with its configuration, not running yet. It runs at its own
/// sample rate, whatever the renderer's.
pub struct Input {
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
}

impl Input {
    /// The system's default input device.
    pub fn default_device() -> Result<Self, DeviceError> {
        Self::of(find_device(Direction::Input, None)?)
    }

    /// The input device called `name` (one of [`names`](Self::names)).
    pub fn named(name: &str) -> Result<Self, DeviceError> {
        Self::of(find_device(Direction::Input, Some(name))?)
    }

    /// The names of the input devices the system has now.
    pub fn names() -> Result<Vec<String>, DeviceError> {
        crate::device_names(Direction::Input)
    }

    fn of(device: cpal::Device) -> Result<Self, DeviceError> {
        let supported = device
            .default_input_config()
            .map_err(|err| DeviceError::Backend(err.to_string()))?;
        Ok(Self {
            device,
            format: supported.sample_format(),
            config: supported.config(),
        })
    }

    /// Samples per second per channel.
    pub fn sample_rate(&self) -> f32 {
        self.config.sample_rate as f32
    }

    /// Number of channels per frame.
    pub fn channels(&self) -> usize {
        usize::from(self.config.channels)
    }

    /// Starts the device: from now on its frames go to the returned [`DeviceInput`],
    /// resampled to `sample_rate` (the renderer's), which the renderer reads
    /// ([`Renderer::set_input`](v_signal_nodes_runtime::realtime::Renderer::set_input)).
    /// Runs until the returned stream is dropped; what ends it later arrives on
    /// `errors`.
    pub fn start(
        self,
        sample_rate: f32,
        errors: Sender<DeviceError>,
    ) -> Result<(InputStream, DeviceInput), DeviceError> {
        let channels = self.channels().max(1);
        let ring = (self.sample_rate() * RING).ceil() as usize * channels;
        let (producer, consumer) = RingBuffer::new(ring);
        let input = DeviceInput::new(consumer, channels, self.sample_rate(), sample_rate)?;
        let stream = match self.format {
            SampleFormat::F32 => self.start_as::<f32>(producer, errors),
            SampleFormat::F64 => self.start_as::<f64>(producer, errors),
            SampleFormat::I16 => self.start_as::<i16>(producer, errors),
            SampleFormat::U16 => self.start_as::<u16>(producer, errors),
            SampleFormat::I32 => self.start_as::<i32>(producer, errors),
            SampleFormat::U32 => self.start_as::<u32>(producer, errors),
            SampleFormat::I8 => self.start_as::<i8>(producer, errors),
            SampleFormat::U8 => self.start_as::<u8>(producer, errors),
            format => Err(DeviceError::UnsupportedFormat(format.to_string())),
        }?;
        Ok((stream, input))
    }

    fn start_as<T>(
        self,
        mut ring: Producer<f32>,
        errors: Sender<DeviceError>,
    ) -> Result<InputStream, DeviceError>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let channels = self.channels().max(1);
        let stream = self
            .device
            .build_input_stream::<T, _, _>(
                self.config,
                move |device: &[T], _| {
                    // Whole frames that fit; the rest is lost.
                    let count = device.len().min(ring.slots() / channels * channels);
                    if let Ok(mut chunk) = ring.write_chunk(count) {
                        let (first, second) = chunk.as_mut_slices();
                        for (out, &sample) in first.iter_mut().chain(second).zip(device) {
                            *out = sample.to_sample();
                        }
                        chunk.commit_all();
                    }
                },
                move |err: cpal::Error| match err.kind() {
                    ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {}
                    _ => {
                        let _ = errors.send(DeviceError::Backend(err.to_string()));
                    }
                },
                None,
            )
            .map_err(|err| DeviceError::Backend(err.to_string()))?;
        stream
            .play()
            .map_err(|err| DeviceError::Backend(err.to_string()))?;
        Ok(InputStream { _stream: stream })
    }
}

/// A running input device; dropping it stops the device. What it takes in goes to
/// its [`DeviceInput`].
pub struct InputStream {
    _stream: cpal::Stream,
}

/// What an input device takes in, at the renderer's rate: the renderer's
/// [`InputSource`]. Allocates nothing.
pub struct DeviceInput {
    ring: Consumer<f32>,
    channels: usize,
    /// `None` when the device runs at the renderer's rate.
    resampler: Option<Box<Async<f32>>>,
    /// Device frames for the resampler's next chunk, interleaved.
    staging: Vec<f32>,
    /// The last resampled chunk, interleaved: samples `sent..filled` are still to go.
    output: Vec<f32>,
    sent: usize,
    filled: usize,
    /// Samples waiting in the ring past which the oldest are dropped, down to `backlog`.
    max_backlog: usize,
    backlog: usize,
}

impl DeviceInput {
    /// Takes frames of `channels` interleaved samples from `ring`, which a device
    /// fills at `device_rate`, and gives them at `sample_rate` (the renderer's).
    /// Fails only if the rates are unusable.
    pub fn new(
        ring: Consumer<f32>,
        channels: usize,
        device_rate: f32,
        sample_rate: f32,
    ) -> Result<Self, DeviceError> {
        let channels = channels.max(1);
        let resampler = if sample_rate == device_rate {
            None
        } else {
            Some(Box::new(sinc_resampler(
                device_rate,
                sample_rate,
                channels,
            )?))
        };
        let max_frames =
            |frames: fn(&Async<f32>) -> usize| resampler.as_deref().map_or(0, frames) * channels;
        let samples = |seconds: f32| (device_rate * seconds).ceil() as usize * channels;
        Ok(Self {
            staging: vec![0.0; max_frames(Async::input_frames_max)],
            output: vec![0.0; max_frames(Async::output_frames_max)],
            resampler,
            ring,
            channels,
            max_backlog: samples(MAX_BACKLOG),
            backlog: samples(BACKLOG),
            sent: 0,
            filled: 0,
        })
    }

    /// Fills `samples` with the device's next frames, oldest first; silence for what
    /// isn't there yet.
    fn take(&mut self, samples: &mut [f32]) {
        let waiting = self.ring.slots();
        if waiting > self.max_backlog {
            let dropped = (waiting - self.backlog) / self.channels * self.channels;
            if let Ok(chunk) = self.ring.read_chunk(dropped) {
                chunk.commit_all();
            }
        }
        let count = samples.len().min(self.ring.slots());
        if let Ok(chunk) = self.ring.read_chunk(count) {
            let (first, second) = chunk.as_slices();
            samples[..first.len()].copy_from_slice(first);
            samples[first.len()..count].copy_from_slice(second);
            chunk.commit_all();
        }
        samples[count..].fill(0.0);
    }
}

impl InputSource for DeviceInput {
    fn channels(&self) -> usize {
        self.channels
    }

    fn read(&mut self, samples: &mut [f32]) {
        let Some(mut resampler) = self.resampler.take() else {
            self.take(samples);
            return;
        };
        let mut written = 0;
        while written < samples.len() {
            if self.sent == self.filled {
                let frames_in = resampler.input_frames_next();
                let mut staging = std::mem::take(&mut self.staging);
                self.take(&mut staging[..frames_in * self.channels]);
                self.filled =
                    resample_chunk(&mut resampler, &staging, &mut self.output, self.channels);
                self.staging = staging;
                self.sent = 0;
            }
            let count = (self.filled - self.sent).min(samples.len() - written);
            samples[written..written + count]
                .copy_from_slice(&self.output[self.sent..self.sent + count]);
            self.sent += count;
            written += count;
        }
        self.resampler = Some(resampler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`DeviceInput`] fed by hand: the producer stands in for the device.
    fn device_input(device_rate: f32, rate: f32, channels: usize) -> (Producer<f32>, DeviceInput) {
        let (producer, consumer) = RingBuffer::new(48_000 * channels);
        let mut input = DeviceInput::new(consumer, channels, device_rate, rate).unwrap();
        // Short, so the tests needn't push much.
        input.max_backlog = 1000 * channels;
        input.backlog = 100 * channels;
        (producer, input)
    }

    #[test]
    fn frames_come_out_in_order_and_missing_ones_are_silent() {
        let (mut device, mut input) = device_input(48_000.0, 48_000.0, 2);
        for sample in [1.0, 2.0, 3.0, 4.0] {
            device.push(sample).unwrap();
        }

        let mut samples = [9.0; 6];
        input.read(&mut samples);

        assert_eq!(samples, [1.0, 2.0, 3.0, 4.0, 0.0, 0.0]);
    }

    #[test]
    fn a_growing_backlog_is_dropped_to_keep_the_delay_short() {
        let (mut device, mut input) = device_input(48_000.0, 48_000.0, 1);
        for frame in 0..2000 {
            device.push(frame as f32).unwrap();
        }

        let mut samples = [0.0; 1];
        input.read(&mut samples);

        assert_eq!(samples, [1900.0], "only the newest 100 frames stay");
        assert_eq!(input.ring.slots(), 99);
    }

    #[test]
    fn another_rate_is_resampled_to_the_renderers() {
        // A 1 kHz sine at 44.1 kHz, read at 48 kHz.
        let (mut device, mut input) = device_input(44_100.0, 48_000.0, 1);
        input.max_backlog = usize::MAX;
        for frame in 0..44_100 {
            let time = frame as f32 / 44_100.0;
            device
                .push((std::f32::consts::TAU * 1000.0 * time).sin())
                .unwrap();
        }

        let mut samples = vec![0.0; 24_000];
        for piece in samples.chunks_mut(37) {
            input.read(piece);
        }

        // Past the filter's delay: 1000 rising zero crossings per second.
        let settled = &samples[2000..];
        let crossings = settled
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count();
        let frequency = crossings as f32 * 48_000.0 / settled.len() as f32;
        assert!((frequency - 1000.0).abs() < 5.0, "{frequency}");
    }
}

//! The first device driver: the system's sound devices through `cpal`, the default
//! ones or any by name ([`Output::names`], [`Input::names`]).
//!
//! It only moves samples: [`Output::start`] gives the device's data callback a
//! [`Renderer`], which does the real work on the device's real-time thread. The
//! callback renders into a fixed `f32` buffer and converts to whatever sample format
//! the device takes, allocating nothing. Everything about graphs and plans stays in
//! `runtime`, testable without a device. An [`Input`] device feeds the renderer's
//! input nodes through a [`DeviceInput`] (see the [`input`] module).
//!
//! A device runs at its own sample rate. A renderer at another one (the project's)
//! goes through a [`Resampled`] on the way out, and the input through a resampler of
//! its own on the way in.
//!
//! A sound card is what `cpal` reaches, so this is one driver among those to come
//! (ADC/DAC, files, displays), each doing the same with its own hardware.
//!
//! Licenses: on Linux `cpal` links ALSA's `libasound` (LGPL-2.1) dynamically; it
//! isn't part of this program's code. CoreAudio and WASAPI are system APIs.

pub mod input;
mod resample;

use std::fmt;
use std::sync::mpsc::Sender;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use v_signal_nodes_runtime::realtime::Renderer;

pub use input::{DeviceInput, Input, InputStream};
pub use resample::{MAX_SINC_LEN, Resampled};

/// Which way a device moves samples.
#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

/// The system's default device of `direction`, or the one called `name`.
fn find_device(direction: Direction, name: Option<&str>) -> Result<cpal::Device, DeviceError> {
    let host = cpal::default_host();
    let Some(name) = name else {
        let default = match direction {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        };
        return default.ok_or(DeviceError::NoDevice);
    };
    devices(&host, direction)?
        .find(|device| device.to_string() == name)
        .ok_or_else(|| DeviceError::NotFound(name.to_owned()))
}

/// The names of the devices of `direction` the system has now.
fn device_names(direction: Direction) -> Result<Vec<String>, DeviceError> {
    let host = cpal::default_host();
    Ok(devices(&host, direction)?
        .map(|device| device.to_string())
        .collect())
}

fn devices(
    host: &cpal::Host,
    direction: Direction,
) -> Result<Box<dyn Iterator<Item = cpal::Device>>, DeviceError> {
    let backend = |err: cpal::Error| DeviceError::Backend(err.to_string());
    Ok(match direction {
        Direction::Input => Box::new(host.input_devices().map_err(backend)?),
        Direction::Output => Box::new(host.output_devices().map_err(backend)?),
    })
}

/// Frames of the `f32` buffer between the renderer and the device's format; longer
/// device blocks are rendered piece by piece.
const SCRATCH_FRAMES: usize = 4096;

/// Why the device can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceError {
    /// The system has no default device of the kind asked for.
    NoDevice,
    /// No device has this name (any more).
    NotFound(String),
    /// The device can't take any of the sample formats supported here.
    UnsupportedFormat(String),
    /// The system's message about a failed call.
    Backend(String),
    /// No resampler from the renderer's sample rate to the device's.
    Resampling(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => f.write_str("no default device"),
            Self::NotFound(name) => write!(f, "no device '{name}'"),
            Self::UnsupportedFormat(format) => {
                write!(f, "unsupported device sample format {format}")
            }
            Self::Backend(message) => f.write_str(message),
            Self::Resampling(message) => write!(f, "can't resample for the device: {message}"),
        }
    }
}

impl std::error::Error for DeviceError {}

/// An output device with its configuration, not running yet. It runs at its own
/// sample rate, whatever the renderer's.
pub struct Output {
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
}

impl Output {
    /// The system's default output device.
    pub fn default_device() -> Result<Self, DeviceError> {
        Self::of(find_device(Direction::Output, None)?)
    }

    /// The output device called `name` (one of [`names`](Self::names)).
    pub fn named(name: &str) -> Result<Self, DeviceError> {
        Self::of(find_device(Direction::Output, Some(name))?)
    }

    /// The names of the output devices the system has now.
    pub fn names() -> Result<Vec<String>, DeviceError> {
        device_names(Direction::Output)
    }

    fn of(device: cpal::Device) -> Result<Self, DeviceError> {
        let supported = device
            .default_output_config()
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

    /// Starts the device: from now on its thread calls `renderer` for every block,
    /// through a [`Resampled`] if the renderer's sample rate isn't the device's. Runs
    /// until the returned stream is dropped. What ends the stream later (a device
    /// unplugged) arrives on `errors`; passing glitches don't.
    pub fn start(
        self,
        renderer: Renderer,
        errors: Sender<DeviceError>,
    ) -> Result<OutputStream, DeviceError> {
        let feed = if renderer.sample_rate() == self.sample_rate() {
            Feed::Direct(renderer)
        } else {
            Feed::Resampled(Box::new(Resampled::new(
                renderer,
                self.sample_rate(),
                self.channels(),
            )?))
        };
        match self.format {
            SampleFormat::F32 => self.start_as::<f32>(feed, errors),
            SampleFormat::F64 => self.start_as::<f64>(feed, errors),
            SampleFormat::I16 => self.start_as::<i16>(feed, errors),
            SampleFormat::U16 => self.start_as::<u16>(feed, errors),
            SampleFormat::I32 => self.start_as::<i32>(feed, errors),
            SampleFormat::U32 => self.start_as::<u32>(feed, errors),
            SampleFormat::I8 => self.start_as::<i8>(feed, errors),
            SampleFormat::U8 => self.start_as::<u8>(feed, errors),
            format => Err(DeviceError::UnsupportedFormat(format.to_string())),
        }
    }

    fn start_as<T>(
        self,
        mut feed: Feed,
        errors: Sender<DeviceError>,
    ) -> Result<OutputStream, DeviceError>
    where
        T: SizedSample + FromSample<f32>,
    {
        let channels = self.channels().max(1);
        let mut scratch = vec![0.0; SCRATCH_FRAMES * channels];
        let stream = self
            .device
            .build_output_stream::<T, _, _>(
                self.config,
                move |device: &mut [T], _| {
                    for block in device.chunks_mut(scratch.len()) {
                        let samples = &mut scratch[..block.len()];
                        feed.render(samples, channels);
                        for (out, &sample) in block.iter_mut().zip(samples.iter()) {
                            *out = sample.to_sample();
                        }
                    }
                },
                move |err: cpal::Error| match err.kind() {
                    // A glitch or a new route: the stream goes on.
                    ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {}
                    // The receiver is gone only when the run is over.
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
        Ok(OutputStream { _stream: stream })
    }
}

/// Where the device's callback gets its samples.
enum Feed {
    /// The renderer runs at the device's rate.
    Direct(Renderer),
    Resampled(Box<Resampled>),
}

impl Feed {
    fn render(&mut self, device: &mut [f32], channels: usize) {
        match self {
            Self::Direct(renderer) => renderer.render(device, channels),
            Self::Resampled(resampled) => resampled.render(device),
        }
    }
}

/// A running device; dropping it stops the device and drops the renderer.
pub struct OutputStream {
    _stream: cpal::Stream,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use v_signal_nodes_runtime::realtime;

    /// Opens the default input device and takes what it gives for a while: the
    /// ring fills and nothing ends the stream. Ignored like the test below; on macOS
    /// it needs the microphone permission.
    #[test]
    #[ignore = "needs an input device"]
    fn starts_the_default_input_device() {
        use v_signal_nodes_runtime::realtime::InputSource;
        let input = Input::default_device().unwrap();
        assert!(input.sample_rate() > 0.0);
        let (errors, received) = mpsc::channel();

        let (stream, mut source) = input.start(48_000.0, errors).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let mut samples = vec![9.0; 64 * source.channels()];
        source.read(&mut samples);
        drop(stream);

        assert!(samples.iter().all(|sample| (-1.0..=1.0).contains(sample)));
        assert_eq!(received.try_recv().ok(), None);
        assert!(Input::names().unwrap().iter().any(|name| !name.is_empty()));
    }

    /// Opens the real device, silently: nothing runs, so the renderer outputs zeros.
    /// Ignored because CI machines have no device.
    #[test]
    #[ignore = "needs an output device"]
    fn starts_the_default_device_silently() {
        let output = Output::default_device().unwrap();
        assert!(output.sample_rate() > 0.0);
        assert!(output.channels() > 0);
        let (_controller, renderer) = realtime::channel(output.sample_rate(), 64);
        let (errors, received) = mpsc::channel();

        let stream = output.start(renderer, errors).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(stream);

        assert_eq!(received.try_recv().ok(), None);
    }
}

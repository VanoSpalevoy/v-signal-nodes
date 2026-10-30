//! The first device driver: the system's default output through `cpal`.
//!
//! It only moves samples: [`Output::start`] gives the device's data callback a
//! [`Renderer`], which does the real work on the device's real-time thread. The
//! callback renders into a fixed `f32` buffer and converts to whatever sample format
//! the device takes, allocating nothing. Everything about graphs and plans stays in
//! `runtime`, testable without a device.
//!
//! A sound card is what `cpal` reaches, so this is one driver among those to come
//! (ADC/DAC, files, displays), each doing the same with its own hardware.
//!
//! Licenses: on Linux `cpal` links ALSA's `libasound` (LGPL-2.1) dynamically; it
//! isn't part of this program's code. CoreAudio and WASAPI are system APIs.

use std::fmt;
use std::sync::mpsc::Sender;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use v_signal_nodes_runtime::realtime::Renderer;

/// Frames of the `f32` buffer between the renderer and the device's format; longer
/// device blocks are rendered piece by piece.
const SCRATCH_FRAMES: usize = 4096;

/// Why the device can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceError {
    /// The system has no default output device.
    NoDevice,
    /// The device can't take any of the sample formats supported here.
    UnsupportedFormat(String),
    /// The system's message about a failed call.
    Backend(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => f.write_str("no default output device"),
            Self::UnsupportedFormat(format) => {
                write!(f, "unsupported device sample format {format}")
            }
            Self::Backend(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DeviceError {}

/// The default output device with its configuration, not running yet. Its sample
/// rate is what the engines for it have to be prepared with.
pub struct Output {
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
}

impl Output {
    /// Finds the system's default output device.
    pub fn default_device() -> Result<Self, DeviceError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(DeviceError::NoDevice)?;
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

    /// Starts the device: from now on its thread calls `renderer` for every block.
    /// Runs until the returned stream is dropped. What ends the stream later (a device
    /// unplugged) arrives on `errors`; passing glitches don't.
    pub fn start(
        self,
        renderer: Renderer,
        errors: Sender<DeviceError>,
    ) -> Result<OutputStream, DeviceError> {
        match self.format {
            SampleFormat::F32 => self.start_as::<f32>(renderer, errors),
            SampleFormat::F64 => self.start_as::<f64>(renderer, errors),
            SampleFormat::I16 => self.start_as::<i16>(renderer, errors),
            SampleFormat::U16 => self.start_as::<u16>(renderer, errors),
            SampleFormat::I32 => self.start_as::<i32>(renderer, errors),
            SampleFormat::U32 => self.start_as::<u32>(renderer, errors),
            SampleFormat::I8 => self.start_as::<i8>(renderer, errors),
            SampleFormat::U8 => self.start_as::<u8>(renderer, errors),
            format => Err(DeviceError::UnsupportedFormat(format.to_string())),
        }
    }

    fn start_as<T>(
        self,
        mut renderer: Renderer,
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
                        renderer.render(samples, channels);
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

//! The API a plugin implements to provide node types.
//!
//! A [`Plugin`] registers one [`NodeProvider`] per node type through a
//! [`Registrar`]. A provider describes its node type with a [`NodeSpec`] and
//! creates a fresh [`NodeProcessor`] (the DSP state) for every node instance.
//!
//! The same traits serve both ways of loading a plugin:
//!
//! - **in-process** — the host links the plugin crate and passes the [`Plugin`]
//!   value to `PluginHost::load`;
//! - **dynamic library** — the plugin crate is built as a `cdylib` and calls
//!   [`export_plugin!`]; the host loads it with `PluginHost::load_library`. Rust
//!   trait objects aren't ABI-stable, so everything crosses the library boundary
//!   through the C ABI in [`ffi`], and the host wraps it back into these traits.
//!
//! # The contract
//!
//! What a plugin builds against is this crate's own items plus the spec types it
//! re-exports from `v-signal-nodes-core`: [`NodeSpec`] and what it's made of
//! ([`NodeKind`], [`Rgb`], [`NodeSocket`], [`SocketDirection`], [`SignalRate`],
//! [`MAX_CHANNELS`], [`ParamSpec`], [`ParamKind`], [`TextKey`], [`Translation`]),
//! [`ParamValue`] and the typed parameter keys ([`ParamKey`], [`NumberParam`],
//! [`ChoiceParam`], [`ToggleParam`]). A change to any of them is a change to the
//! plugin API. The rest of `core` (the graph-side `Node`, `NodeFactory`,
//! `AppError`) is the host's business and isn't exposed; plugins report errors
//! with [`PluginError`]. Dynamic libraries don't see the Rust types at all, only
//! their C layout in [`ffi`], so those types may change without breaking the ABI
//! as long as [`ffi`] doesn't.
//!
//! Parameters are declared once as key constants; the spec and the reads in
//! `process` both use them, so they can't disagree on the id or the kind:
//!
//! ```ignore
//! use v_signal_nodes_plugin_api::*;
//!
//! const GAIN: NumberParam = NumberParam("gain");
//!
//! struct MyGain;
//!
//! impl NodeProvider for MyGain {
//!     fn spec(&self) -> NodeSpec {
//!         NodeSpec::new("my.gain", "Gain", NodeKind::Processor)
//!             .input(NodeSocket::signal_in("In"))
//!             .output(NodeSocket::signal_out("Out"))
//!             .param(GAIN.spec("Gain", 0.0, 2.0, 1.0))
//!     }
//!     fn create_processor(&self) -> Box<dyn NodeProcessor> { Box::new(MyGainProcessor) }
//! }
//!
//! impl NodeProcessor for MyGainProcessor {
//!     fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError> {
//!         let gain = io.get(GAIN)?;
//!         let input = io.input(0);
//!         for (out, sample) in io.output(0).iter_mut().zip(input) {
//!             *out = sample * gain;
//!         }
//!         Ok(())
//!     }
//! }
//!
//! struct MyPlugin;
//!
//! impl Plugin for MyPlugin {
//!     fn name(&self) -> &str { "my" }
//!     fn version(&self) -> &str { env!("CARGO_PKG_VERSION") }
//!     fn register(&self, registrar: &mut dyn Registrar) {
//!         registrar.register_node(Box::new(MyGain));
//!     }
//! }
//!
//! export_plugin!(MyPlugin);
//! ```

// This is the contract plugin authors build against: everything public is documented.
#![warn(missing_docs)]

use std::fmt;
use std::marker::PhantomData;
use std::slice;

pub mod ffi;

pub use v_signal_nodes_core::{
    ChoiceParam, MAX_CHANNELS, NodeKind, NodeSocket, NodeSpec, NumberParam, ParamKey, ParamKind,
    ParamSpec, ParamValue, Rgb, SignalRate, SocketDirection, TextKey, ToggleParam, Translation,
};

/// Version of this API. The host refuses dynamic libraries built against another
/// version (in-process plugins are compiled against the host's copy of this crate,
/// so they always match).
///
/// Bump it on any change to the traits or to the [`ffi`] types.
pub const API_VERSION: u32 = 3;

/// An error reported by a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError(String);

impl PluginError {
    /// An error with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// What went wrong.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PluginError {}

impl From<PluginError> for v_signal_nodes_core::AppError {
    fn from(err: PluginError) -> Self {
        Self::Plugin(err.0)
    }
}

/// Per-instance processing state of a node.
///
/// The host calls [`prepare`](Self::prepare) before the first
/// [`process`](Self::process) and again whenever the sample rate or the block
/// size changes, then `process` with at most `max_frames` frames per call, and
/// [`reset`](Self::reset) when playback jumps (stop, seek), so stale state such
/// as an oscillator phase or a filter tail doesn't leak into the new position.
pub trait NodeProcessor: Send {
    /// Gets ready to process at `sample_rate` samples per second, in blocks of at
    /// most `max_frames`. The place to allocate buffers: `process` runs on the
    /// audio thread and shouldn't. Does nothing by default.
    fn prepare(&mut self, sample_rate: f32, max_frames: usize) -> Result<(), PluginError> {
        let _ = (sample_rate, max_frames);
        Ok(())
    }

    /// Forgets the signal history, keeping what `prepare` set up. Does nothing by
    /// default.
    fn reset(&mut self) {}

    /// Processes one block. See [`ProcessIo`] for what's available.
    fn process(&mut self, io: &mut ProcessIo<'_>) -> Result<(), PluginError>;
}

/// One node type offered by a plugin.
pub trait NodeProvider: Send + Sync {
    /// The node type's description. Its `type_id` should be namespaced by the
    /// plugin, e.g. `"std.gain"`.
    fn spec(&self) -> NodeSpec;

    /// Creates the processing state for a new node instance.
    fn create_processor(&self) -> Box<dyn NodeProcessor>;
}

/// Collects the node types a plugin offers.
pub trait Registrar {
    /// Adds one node type.
    fn register_node(&mut self, provider: Box<dyn NodeProvider>);
}

/// A plugin: a named bundle of node types.
pub trait Plugin {
    /// Unique plugin name, e.g. `"std"`.
    fn name(&self) -> &str;

    /// The plugin's own version, as `major.minor.patch` (usually
    /// `env!("CARGO_PKG_VERSION")`). Documents record it, so a file can tell
    /// which plugin version its nodes and parameter values came from.
    fn version(&self) -> &str;

    /// Registers all node types of the plugin.
    fn register(&self, registrar: &mut dyn Registrar);
}

/// A sample buffer passed to [`NodeProcessor::process`], as a raw pointer and
/// length so it can cross the C ABI unchanged.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RawBuffer {
    /// First sample.
    pub ptr: *mut f32,
    /// Number of samples.
    pub len: usize,
}

impl RawBuffer {
    /// Points at `samples` (read-only use: an input).
    pub fn input(samples: &[f32]) -> Self {
        Self {
            ptr: samples.as_ptr().cast_mut(),
            len: samples.len(),
        }
    }

    /// Points at `samples` (writable: an output).
    pub fn output(samples: &mut [f32]) -> Self {
        Self {
            ptr: samples.as_mut_ptr(),
            len: samples.len(),
        }
    }
}

/// Everything a node gets for one [`process`](NodeProcessor::process) call:
/// parameter values, input samples and output buffers for a block of `frames`.
///
/// Inputs and outputs are in socket order of the node's [`NodeSpec`]; parameters in
/// the order of [`NodeSpec::params`], read by key with [`get`](Self::get). Each
/// socket's buffer holds its channels one after another, each
/// [`NodeSocket::channel_len`] long (`frames` at audio rate, one value at control
/// rate); [`input_channels`](Self::input_channels) /
/// [`output_channels`](Self::output_channels) split it. Outputs start zeroed. The
/// sample rate isn't here: it comes with [`prepare`](NodeProcessor::prepare).
pub struct ProcessIo<'a> {
    frames: usize,
    spec: &'a NodeSpec,
    params: &'a [ParamValue],
    inputs: &'a [RawBuffer],
    outputs: &'a [RawBuffer],
    _samples: PhantomData<&'a mut [f32]>,
}

impl<'a> ProcessIo<'a> {
    /// # Safety
    ///
    /// `inputs` and `outputs` must match `spec`'s sockets in number, and for `'a`
    /// every buffer must point to [`NodeSocket::buffer_len`]`(frames)` initialized
    /// samples of its socket. The outputs must be writable, and no output may
    /// overlap another output or any input.
    pub unsafe fn from_raw(
        frames: usize,
        spec: &'a NodeSpec,
        params: &'a [ParamValue],
        inputs: &'a [RawBuffer],
        outputs: &'a [RawBuffer],
    ) -> Self {
        debug_assert_eq!(inputs.len(), spec.inputs.len());
        debug_assert_eq!(outputs.len(), spec.outputs.len());
        Self {
            frames,
            spec,
            params,
            inputs,
            outputs,
            _samples: PhantomData,
        }
    }

    /// Number of frames (samples per audio-rate channel) in this block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The node's type.
    pub fn spec(&self) -> &'a NodeSpec {
        self.spec
    }

    /// All parameter values, in spec order.
    pub fn params(&self) -> &'a [ParamValue] {
        self.params
    }

    /// The value of parameter `key`. Fails if the spec has no parameter with the
    /// key's id or its value is of another kind: a plugin bug (the key wasn't
    /// used to build the spec), reported instead of a silent default.
    pub fn get<K: ParamKey>(&self, key: K) -> Result<K::Value, PluginError> {
        self.spec
            .param_index(key.id())
            .and_then(|index| self.params.get(index))
            .and_then(|&value| key.read(value))
            .ok_or_else(|| {
                PluginError::new(format!(
                    "parameter '{}' is missing or of another kind",
                    key.id()
                ))
            })
    }

    /// Number of inputs.
    pub fn input_count(&self) -> usize {
        self.inputs.len()
    }

    /// Number of outputs.
    pub fn output_count(&self) -> usize {
        self.outputs.len()
    }

    /// Buffer of input `index`, all channels. Panics if there's no such input.
    ///
    /// Not tied to `&self`, so an input can be read while an output is written.
    pub fn input(&self, index: usize) -> &'a [f32] {
        let buffer = self.inputs[index];
        // SAFETY: `from_raw` guarantees a valid, initialized buffer for `'a` that
        // no output overlaps, so a shared slice can't alias a `&mut`.
        unsafe { slice_from_raw(buffer.ptr, buffer.len) }
    }

    /// The channels of input `index`, in order. Panics if there's no such input.
    pub fn input_channels(&self, index: usize) -> slice::ChunksExact<'a, f32> {
        let len = self.spec.inputs[index].channel_len(self.frames);
        // An empty block has no values to split; `max` keeps the chunk size valid.
        self.input(index).chunks_exact(len.max(1))
    }

    /// Buffer of output `index`, all channels. Panics if there's no such output.
    pub fn output(&mut self, index: usize) -> &mut [f32] {
        let buffer = self.outputs[index];
        // SAFETY: `from_raw` guarantees a valid, writable buffer for `'a` that
        // overlaps nothing else; borrowing `self` mutably hands out one at a time.
        unsafe {
            if buffer.len == 0 {
                &mut []
            } else {
                slice::from_raw_parts_mut(buffer.ptr, buffer.len)
            }
        }
    }

    /// The channels of output `index`, in order. Panics if there's no such output.
    pub fn output_channels(&mut self, index: usize) -> slice::ChunksExactMut<'_, f32> {
        let len = self.spec.outputs[index].channel_len(self.frames);
        self.output(index).chunks_exact_mut(len.max(1))
    }

    /// The raw parts, for hosts that forward the call (e.g. across the C ABI).
    pub fn raw_parts(&mut self) -> (usize, &'a [ParamValue], &'a [RawBuffer], &'a [RawBuffer]) {
        (self.frames, self.params, self.inputs, self.outputs)
    }
}

/// `slice::from_raw_parts` that accepts a null/dangling pointer for an empty slice.
///
/// # Safety
///
/// Same as [`slice::from_raw_parts`] when `len > 0`.
unsafe fn slice_from_raw<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: forwarded to the caller.
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

/// Exports a plugin from a `cdylib` crate, so the host can load it with
/// `PluginHost::load_library`.
///
/// Takes an expression that builds the plugin (a `'static` [`Plugin`]). Call it
/// once per library, at the crate root; it defines the entry symbols named in
/// [`ffi`].
#[macro_export]
macro_rules! export_plugin {
    ($plugin:expr) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn v_signal_nodes_plugin_api_version() -> u32 {
            $crate::API_VERSION
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn v_signal_nodes_plugin_entry() -> $crate::ffi::FfiPlugin {
            $crate::ffi::export(|| $plugin)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEVEL: NumberParam = NumberParam("level");
    const ON: ToggleParam = ToggleParam("on");

    /// A stereo audio input, a 2-channel control input and a stereo output.
    fn spec() -> NodeSpec {
        NodeSpec::new("test.io", "IO", NodeKind::Processor)
            .input(NodeSocket::signal_in("audio").with_channels(2))
            .input(NodeSocket::control_in("control").with_channels(2))
            .output(NodeSocket::signal_out("out").with_channels(2))
            .param(LEVEL.spec("Level", 0.0, 1.0, 0.5))
    }

    #[test]
    fn splits_buffers_into_channels_and_reads_params_by_key() {
        let spec = spec();
        let audio = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let control = [0.25, 0.75];
        let mut out = [0.0; 6];
        let inputs = [RawBuffer::input(&audio), RawBuffer::input(&control)];
        let outputs = [RawBuffer::output(&mut out)];
        let params = [ParamValue::Number(0.5)];
        // SAFETY: buffers sized by `buffer_len(3)` of each socket, live for the
        // call, the output overlaps nothing.
        let mut io = unsafe { ProcessIo::from_raw(3, &spec, &params, &inputs, &outputs) };

        let channels: Vec<_> = io.input_channels(0).collect();
        assert_eq!(channels, [&[1.0, 2.0, 3.0][..], &[4.0, 5.0, 6.0]]);
        let control: Vec<_> = io.input_channels(1).collect();
        assert_eq!(control, [&[0.25][..], &[0.75]]);
        assert_eq!(io.get(LEVEL), Ok(0.5));
        assert!(io.get(ON).is_err());
        assert!(io.get(NumberParam("missing")).is_err());

        for (index, channel) in io.output_channels(0).enumerate() {
            channel.fill(index as f32 + 1.0);
        }
        assert_eq!(out, [1.0, 1.0, 1.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn an_empty_block_has_no_channels_to_split() {
        let spec = spec();
        let inputs = [RawBuffer::input(&[]), RawBuffer::input(&[0.0, 0.0])];
        let outputs = [RawBuffer::output(&mut [])];
        // SAFETY: zero frames: empty audio buffers, one value per control channel.
        let mut io = unsafe { ProcessIo::from_raw(0, &spec, &[], &inputs, &outputs) };

        assert_eq!(io.input_channels(0).count(), 0);
        assert_eq!(io.input_channels(1).count(), 2);
        assert_eq!(io.output_channels(0).count(), 0);
    }
}

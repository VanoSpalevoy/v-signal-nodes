//! The C ABI between the host and plugins loaded from dynamic libraries.
//!
//! Plugin authors don't use this module directly: [`export_plugin!`](crate::export_plugin)
//! wraps a [`Plugin`] into it, and the host wraps it back.
//!
//! # Protocol
//!
//! 1. The host looks up [`VERSION_SYMBOL`] ([`VersionFn`]) and refuses the library
//!    unless it returns [`API_VERSION`](crate::API_VERSION).
//! 2. It calls [`ENTRY_SYMBOL`] ([`EntryFn`]) once and gets an [`FfiPlugin`]: an
//!    opaque `ctx` plus function pointers taking it. A null `ctx` means the plugin
//!    failed to initialize.
//! 3. It reads the name and every node spec once and copies them
//!    ([`read_spec`]); borrowed data (`FfiStr`, `FfiSlice`) is only valid until
//!    the next call on the same object.
//! 4. Per node instance it calls `create_processor` and drives the returned
//!    [`FfiProcessor`] as the [`NodeProcessor`] contract says (`prepare` before
//!    `process`); it calls each object's `drop` exactly once, processors before
//!    the plugin, and keeps the library loaded until then.
//!
//! Each side frees only what it allocated. Panics never cross the boundary: the
//! plugin side catches them and reports an error (or a null `ctx`) instead. This
//! needs the plugin built with `panic = "unwind"` (the default): with
//! `panic = "abort"` a panic in a plugin aborts the whole host process.
//!
//! Only types defined here cross the boundary, never Rust types from `core`, so
//! changing those can't silently change the ABI. Enums travel as `u8` codes and
//! are checked when read; there is no `bool` (a C plugin could pass a value Rust
//! doesn't allow), flags are `u8` with non-zero meaning true.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Arc;

use crate::{
    NodeKind, NodeProcessor, NodeProvider, NodeSocket, NodeSpec, ParamKind, ParamSpec, ParamValue,
    Plugin, PluginError, ProcessIo, RawBuffer, Registrar, Rgb, SignalRate, SocketDirection,
    TextKey, Translation, slice_from_raw,
};

/// Name of the exported [`VersionFn`].
pub const VERSION_SYMBOL: &[u8] = b"v_signal_nodes_plugin_api_version\0";
/// Name of the exported [`EntryFn`].
pub const ENTRY_SYMBOL: &[u8] = b"v_signal_nodes_plugin_entry\0";

/// Returns the API version the library was built against.
pub type VersionFn = unsafe extern "C" fn() -> u32;
/// Creates the plugin; called once per load.
pub type EntryFn = unsafe extern "C" fn() -> FfiPlugin;

/// A borrowed slice.
#[repr(C)]
pub struct FfiSlice<T> {
    /// First item; may dangle when `len == 0`.
    pub ptr: *const T,
    /// Number of items.
    pub len: usize,
}

impl<T> Clone for FfiSlice<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for FfiSlice<T> {}

impl<T> FfiSlice<T> {
    /// Borrows `items`.
    pub fn new(items: &[T]) -> Self {
        Self {
            ptr: items.as_ptr(),
            len: items.len(),
        }
    }

    /// # Safety
    ///
    /// `ptr` must point to `len` valid items for `'a` (any pointer if `len == 0`).
    pub unsafe fn as_slice<'a>(self) -> &'a [T] {
        // SAFETY: forwarded to the caller.
        unsafe { slice_from_raw(self.ptr, self.len) }
    }
}

/// A borrowed UTF-8 string (checked when read).
pub type FfiStr = FfiSlice<u8>;

impl FfiStr {
    /// Borrows `text`.
    pub fn of(text: &str) -> Self {
        Self::new(text.as_bytes())
    }

    /// Copies the string out.
    ///
    /// # Safety
    ///
    /// As for [`as_slice`](FfiSlice::as_slice).
    pub unsafe fn read(self) -> Result<String, PluginError> {
        // SAFETY: forwarded to the caller.
        let bytes = unsafe { self.as_slice() };
        String::from_utf8(bytes.to_vec()).map_err(|_| PluginError::new("string is not valid UTF-8"))
    }
}

/// A socket of a [`FfiNodeSpec`]; the direction is given by the list it's in.
#[repr(C)]
pub struct FfiSocket {
    /// Label (English).
    pub name: FfiStr,
    /// [`SignalRate`] code.
    pub rate: u8,
    /// Number of channels (checked by the host's spec validation).
    pub channels: u16,
}

/// A parameter of a [`FfiNodeSpec`]. Which fields matter depends on `kind`:
/// number — `min`, `max`, `default`; choice — `options`, `default` (index);
/// toggle — `default` (non-zero is on).
#[repr(C)]
pub struct FfiParam {
    /// Stable id within the node type.
    pub id: FfiStr,
    /// Label (English).
    pub name: FfiStr,
    /// [`ParamKind`] code.
    pub kind: u8,
    /// Lowest number.
    pub min: f32,
    /// Highest number.
    pub max: f32,
    /// Default number, choice index or toggle state.
    pub default: f32,
    /// Choice options (English).
    pub options: FfiSlice<FfiStr>,
}

/// A [`TextKey`]. Which fields matter depends on `kind`: input/output — `index`;
/// parameter — `param`; option — `param` and `index`.
#[repr(C)]
pub struct FfiTextKey {
    /// [`TextKey`] variant code.
    pub kind: u8,
    /// Socket or option index.
    pub index: usize,
    /// Parameter id.
    pub param: FfiStr,
}

/// A [`Translation`].
#[repr(C)]
pub struct FfiTranslation {
    /// Language code, e.g. `"ru"`.
    pub language: FfiStr,
    /// The label being translated.
    pub key: FfiTextKey,
    /// The translation.
    pub text: FfiStr,
}

/// A [`NodeSpec`].
#[repr(C)]
pub struct FfiNodeSpec {
    /// Namespaced type id, e.g. `"std.gain"`.
    pub type_id: FfiStr,
    /// Title (English).
    pub name: FfiStr,
    /// [`NodeKind`] code.
    pub kind: u8,
    /// Non-zero if `color` is set.
    pub has_color: u8,
    /// The node's own color as `0xRRGGBB`.
    pub color: u32,
    /// Input sockets, in order.
    pub inputs: FfiSlice<FfiSocket>,
    /// Output sockets, in order.
    pub outputs: FfiSlice<FfiSocket>,
    /// Parameters, in order.
    pub params: FfiSlice<FfiParam>,
    /// Translations of the English strings above.
    pub translations: FfiSlice<FfiTranslation>,
}

/// A loaded plugin.
#[repr(C)]
pub struct FfiPlugin {
    /// Opaque plugin state; null if the plugin failed to initialize.
    pub ctx: *mut c_void,
    /// The plugin's name.
    pub name: unsafe extern "C" fn(ctx: *const c_void) -> FfiStr,
    /// The plugin's version.
    pub version: unsafe extern "C" fn(ctx: *const c_void) -> FfiStr,
    /// Number of node types.
    pub node_count: unsafe extern "C" fn(ctx: *const c_void) -> usize,
    /// Spec of node type `index` (`< node_count`).
    pub node_spec: unsafe extern "C" fn(ctx: *const c_void, index: usize) -> FfiNodeSpec,
    /// A new processor for node type `index` (`< node_count`).
    pub create_processor: unsafe extern "C" fn(ctx: *const c_void, index: usize) -> FfiProcessor,
    /// Frees the plugin; called once, after all its processors are dropped.
    pub drop: unsafe extern "C" fn(ctx: *mut c_void),
}

/// A [`ParamValue`]. Which field matters depends on `kind`: number — `number`;
/// choice — `index`; toggle — `index` (non-zero is on).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FfiParamValue {
    /// [`ParamKind`] code.
    pub kind: u8,
    /// The number.
    pub number: f32,
    /// The option index or toggle state.
    pub index: u32,
}

impl From<ParamValue> for FfiParamValue {
    fn from(value: ParamValue) -> Self {
        let (kind, number, index) = match value {
            ParamValue::Number(number) => (PARAM_NUMBER, number, 0),
            // Saturates: no choice has 4 billion options, so it's out of range
            // either way and the plugin side falls back to its default.
            ParamValue::Choice(index) => {
                (PARAM_CHOICE, 0.0, u32::try_from(index).unwrap_or(u32::MAX))
            }
            ParamValue::Toggle(on) => (PARAM_TOGGLE, 0.0, u32::from(on)),
        };
        Self {
            kind,
            number,
            index,
        }
    }
}

impl TryFrom<FfiParamValue> for ParamValue {
    type Error = PluginError;

    fn try_from(value: FfiParamValue) -> Result<Self, PluginError> {
        Ok(match value.kind {
            PARAM_NUMBER => Self::Number(value.number),
            PARAM_CHOICE => Self::Choice(value.index as usize),
            PARAM_TOGGLE => Self::Toggle(value.index != 0),
            code => return Err(bad_code("parameter kind", code)),
        })
    }
}

/// Arguments of [`FfiProcessor::process`]; the parts of a [`ProcessIo`].
#[repr(C)]
pub struct FfiProcessArgs {
    /// Samples in every buffer; at most the `max_frames` of the last `prepare`.
    pub frames: usize,
    /// Parameter values, in spec order.
    pub params: FfiSlice<FfiParamValue>,
    /// Input buffers, in socket order.
    pub inputs: FfiSlice<RawBuffer>,
    /// Output buffers, in socket order.
    pub outputs: FfiSlice<RawBuffer>,
}

/// Result of a processor call; on failure `message` holds the error.
#[repr(C)]
pub struct FfiStatus {
    /// Non-zero if the call succeeded.
    pub ok: u8,
    /// The error if not `ok`; borrowed until the next call on the processor.
    pub message: FfiStr,
}

impl FfiStatus {
    /// A success.
    pub fn ok() -> Self {
        Self {
            ok: 1,
            message: FfiStr::of(""),
        }
    }

    /// A failure with `message`.
    pub fn failed(message: &str) -> Self {
        Self {
            ok: 0,
            message: FfiStr::of(message),
        }
    }

    /// Copies the result out.
    ///
    /// # Safety
    ///
    /// `message` must be valid (see [`FfiSlice::as_slice`]).
    pub unsafe fn read(self) -> Result<(), PluginError> {
        if self.ok != 0 {
            return Ok(());
        }
        // SAFETY: forwarded to the caller.
        Err(PluginError::new(unsafe { self.message.read() }?))
    }
}

/// A node instance's processor; the calls of [`NodeProcessor`].
#[repr(C)]
pub struct FfiProcessor {
    /// Opaque processor state; null if creating it failed.
    pub ctx: *mut c_void,
    /// [`NodeProcessor::prepare`].
    pub prepare:
        unsafe extern "C" fn(ctx: *mut c_void, sample_rate: f32, max_frames: usize) -> FfiStatus,
    /// [`NodeProcessor::reset`].
    pub reset: unsafe extern "C" fn(ctx: *mut c_void),
    /// [`NodeProcessor::process`]. Must be called with arguments that satisfy
    /// [`ProcessIo::from_raw`].
    pub process: unsafe extern "C" fn(ctx: *mut c_void, args: FfiProcessArgs) -> FfiStatus,
    /// Frees the processor; called once.
    pub drop: unsafe extern "C" fn(ctx: *mut c_void),
}

// --- Enum codes -------------------------------------------------------------

fn node_kind_code(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Generator => 0,
        NodeKind::Processor => 1,
        NodeKind::Output => 2,
    }
}

fn node_kind_from(code: u8) -> Result<NodeKind, PluginError> {
    Ok(match code {
        0 => NodeKind::Generator,
        1 => NodeKind::Processor,
        2 => NodeKind::Output,
        _ => return Err(bad_code("node kind", code)),
    })
}

fn rate_code(rate: SignalRate) -> u8 {
    match rate {
        SignalRate::Sample => 0,
        SignalRate::Control => 1,
    }
}

fn rate_from(code: u8) -> Result<SignalRate, PluginError> {
    Ok(match code {
        0 => SignalRate::Sample,
        1 => SignalRate::Control,
        _ => return Err(bad_code("signal rate", code)),
    })
}

fn color_code(color: Option<Rgb>) -> (u8, u32) {
    match color {
        Some(Rgb { r, g, b }) => (1, u32::from_be_bytes([0, r, g, b])),
        None => (0, 0),
    }
}

fn color_from(has_color: u8, color: u32) -> Option<Rgb> {
    let [_, r, g, b] = color.to_be_bytes();
    (has_color != 0).then_some(Rgb { r, g, b })
}

const PARAM_NUMBER: u8 = 0;
const PARAM_CHOICE: u8 = 1;
const PARAM_TOGGLE: u8 = 2;

const TEXT_NAME: u8 = 0;
const TEXT_INPUT: u8 = 1;
const TEXT_OUTPUT: u8 = 2;
const TEXT_PARAM: u8 = 3;
const TEXT_OPTION: u8 = 4;

/// The C layout of `key`, borrowing its parameter id.
fn text_key_ffi(key: &TextKey) -> FfiTextKey {
    let (kind, index, param) = match key {
        TextKey::Name => (TEXT_NAME, 0, ""),
        TextKey::Input(index) => (TEXT_INPUT, *index, ""),
        TextKey::Output(index) => (TEXT_OUTPUT, *index, ""),
        TextKey::Param(id) => (TEXT_PARAM, 0, id.as_str()),
        TextKey::Option(id, index) => (TEXT_OPTION, *index, id.as_str()),
    };
    FfiTextKey {
        kind,
        index,
        param: FfiStr::of(param),
    }
}

/// # Safety
///
/// `key.param` must be valid (see [`FfiSlice::as_slice`]).
unsafe fn text_key_from(key: &FfiTextKey) -> Result<TextKey, PluginError> {
    // SAFETY: forwarded to the caller.
    let param = || unsafe { key.param.read() };
    Ok(match key.kind {
        TEXT_NAME => TextKey::Name,
        TEXT_INPUT => TextKey::Input(key.index),
        TEXT_OUTPUT => TextKey::Output(key.index),
        TEXT_PARAM => TextKey::Param(param()?),
        TEXT_OPTION => TextKey::Option(param()?, key.index),
        code => return Err(bad_code("text key", code)),
    })
}

fn bad_code(what: &str, code: u8) -> PluginError {
    PluginError::new(format!("unknown {what} code {code}"))
}

// --- Host side --------------------------------------------------------------

/// Copies a spec received from a plugin.
///
/// # Safety
///
/// Every `FfiStr`/`FfiSlice` in `spec` must be valid (see [`FfiSlice::as_slice`]).
pub unsafe fn read_spec(spec: &FfiNodeSpec) -> Result<NodeSpec, PluginError> {
    // SAFETY: forwarded to the caller (covers the whole function).
    unsafe {
        let sockets =
            |list: FfiSlice<FfiSocket>, direction| -> Result<Vec<NodeSocket>, PluginError> {
                list.as_slice()
                    .iter()
                    .map(|socket| {
                        let name = socket.name.read()?;
                        let rate = rate_from(socket.rate)?;
                        let socket_of = match direction {
                            SocketDirection::Input => NodeSocket::input,
                            SocketDirection::Output => NodeSocket::output,
                        };
                        Ok(socket_of(name, rate).with_channels(socket.channels))
                    })
                    .collect()
            };
        let params = spec
            .params
            .as_slice()
            .iter()
            .map(|param| {
                let kind = match param.kind {
                    PARAM_NUMBER => ParamKind::Number {
                        min: param.min,
                        max: param.max,
                        default: param.default,
                    },
                    PARAM_CHOICE => ParamKind::Choice {
                        options: param
                            .options
                            .as_slice()
                            .iter()
                            .map(|option| option.read())
                            .collect::<Result<_, _>>()?,
                        default: param.default as usize,
                    },
                    PARAM_TOGGLE => ParamKind::Toggle {
                        default: param.default != 0.0,
                    },
                    code => return Err(bad_code("parameter kind", code)),
                };
                Ok(ParamSpec {
                    id: param.id.read()?,
                    name: param.name.read()?,
                    kind,
                })
            })
            .collect::<Result<_, PluginError>>()?;
        let translations = spec
            .translations
            .as_slice()
            .iter()
            .map(|translation| {
                Ok(Translation {
                    language: translation.language.read()?,
                    key: text_key_from(&translation.key)?,
                    text: translation.text.read()?,
                })
            })
            .collect::<Result<_, PluginError>>()?;

        Ok(NodeSpec {
            type_id: spec.type_id.read()?,
            name: spec.name.read()?,
            kind: node_kind_from(spec.kind)?,
            color: color_from(spec.has_color, spec.color),
            inputs: sockets(spec.inputs, SocketDirection::Input)?,
            outputs: sockets(spec.outputs, SocketDirection::Output)?,
            params,
            translations,
        })
    }
}

// --- Plugin side ------------------------------------------------------------

/// Wraps the plugin built by `make` into the C ABI; used by
/// [`export_plugin!`](crate::export_plugin). Returns a null `ctx` if `make` or
/// the plugin's registration panics.
pub fn export<P: Plugin + 'static>(make: impl FnOnce() -> P) -> FfiPlugin {
    let ctx = catch_unwind(AssertUnwindSafe(|| {
        let plugin = make();
        let mut collector = Collector(Vec::new());
        plugin.register(&mut collector);
        let exported = ExportedPlugin {
            name: plugin.name().to_owned(),
            version: plugin.version().to_owned(),
            nodes: collector.0.into_iter().map(ExportedNode::new).collect(),
        };
        Box::into_raw(Box::new(exported)).cast::<c_void>()
    }))
    .unwrap_or(ptr::null_mut());
    FfiPlugin {
        ctx,
        name: plugin_name,
        version: plugin_version,
        node_count: plugin_node_count,
        node_spec: plugin_node_spec,
        create_processor: plugin_create_processor,
        drop: plugin_drop,
    }
}

struct Collector(Vec<Box<dyn NodeProvider>>);

impl Registrar for Collector {
    fn register_node(&mut self, provider: Box<dyn NodeProvider>) {
        self.0.push(provider);
    }
}

/// Plugin state behind [`FfiPlugin::ctx`].
struct ExportedPlugin {
    name: String,
    version: String,
    nodes: Vec<ExportedNode>,
}

/// One node type with its spec kept alive in C layout.
///
/// The `Ffi*` vectors point into `spec`'s strings and into each other. That is
/// fine because none of them is modified after construction, so their heap
/// buffers never move (moving the struct only moves the vector headers). The
/// spec is shared with the node type's processors, which build their
/// [`ProcessIo`] from it.
struct ExportedNode {
    spec: Arc<NodeSpec>,
    inputs: Vec<FfiSocket>,
    outputs: Vec<FfiSocket>,
    /// Option labels per parameter (empty for non-choice parameters).
    _options: Vec<Vec<FfiStr>>,
    params: Vec<FfiParam>,
    translations: Vec<FfiTranslation>,
    provider: Box<dyn NodeProvider>,
}

impl ExportedNode {
    fn new(provider: Box<dyn NodeProvider>) -> Self {
        let spec = Arc::new(provider.spec());
        let sockets = |sockets: &[NodeSocket]| -> Vec<FfiSocket> {
            sockets
                .iter()
                .map(|socket| FfiSocket {
                    name: FfiStr::of(&socket.name),
                    rate: rate_code(socket.rate),
                    channels: socket.channels,
                })
                .collect()
        };
        let options: Vec<Vec<FfiStr>> = spec
            .params
            .iter()
            .map(|param| match &param.kind {
                ParamKind::Choice { options, .. } => {
                    options.iter().map(|option| FfiStr::of(option)).collect()
                }
                _ => Vec::new(),
            })
            .collect();
        let params = spec
            .params
            .iter()
            .zip(&options)
            .map(|(param, options)| {
                let (kind, min, max, default) = match &param.kind {
                    ParamKind::Number { min, max, default } => (PARAM_NUMBER, *min, *max, *default),
                    ParamKind::Choice { default, .. } => (PARAM_CHOICE, 0.0, 0.0, *default as f32),
                    ParamKind::Toggle { default } => {
                        (PARAM_TOGGLE, 0.0, 0.0, f32::from(u8::from(*default)))
                    }
                };
                FfiParam {
                    id: FfiStr::of(&param.id),
                    name: FfiStr::of(&param.name),
                    kind,
                    min,
                    max,
                    default,
                    options: FfiSlice::new(options),
                }
            })
            .collect();
        let translations = spec
            .translations
            .iter()
            .map(|translation| FfiTranslation {
                language: FfiStr::of(&translation.language),
                key: text_key_ffi(&translation.key),
                text: FfiStr::of(&translation.text),
            })
            .collect();

        Self {
            inputs: sockets(&spec.inputs),
            outputs: sockets(&spec.outputs),
            _options: options,
            params,
            translations,
            spec,
            provider,
        }
    }

    fn ffi_spec(&self) -> FfiNodeSpec {
        let (has_color, color) = color_code(self.spec.color);
        FfiNodeSpec {
            type_id: FfiStr::of(&self.spec.type_id),
            name: FfiStr::of(&self.spec.name),
            kind: node_kind_code(self.spec.kind),
            has_color,
            color,
            inputs: FfiSlice::new(&self.inputs),
            outputs: FfiSlice::new(&self.outputs),
            params: FfiSlice::new(&self.params),
            translations: FfiSlice::new(&self.translations),
        }
    }
}

/// # Safety
///
/// `ctx` must be a live, non-null [`FfiPlugin::ctx`].
unsafe fn plugin<'a>(ctx: *const c_void) -> &'a ExportedPlugin {
    // SAFETY: forwarded to the caller.
    unsafe { &*ctx.cast::<ExportedPlugin>() }
}

unsafe extern "C" fn plugin_name(ctx: *const c_void) -> FfiStr {
    // SAFETY: the host passes the ctx it got from `export`.
    FfiStr::of(unsafe { &plugin(ctx).name })
}

unsafe extern "C" fn plugin_version(ctx: *const c_void) -> FfiStr {
    // SAFETY: as in `plugin_name`.
    FfiStr::of(unsafe { &plugin(ctx).version })
}

unsafe extern "C" fn plugin_node_count(ctx: *const c_void) -> usize {
    // SAFETY: as in `plugin_name`.
    unsafe { plugin(ctx).nodes.len() }
}

unsafe extern "C" fn plugin_node_spec(ctx: *const c_void, index: usize) -> FfiNodeSpec {
    // SAFETY: as in `plugin_name`; the host keeps `index < node_count`.
    unsafe { plugin(ctx).nodes[index].ffi_spec() }
}

unsafe extern "C" fn plugin_create_processor(ctx: *const c_void, index: usize) -> FfiProcessor {
    let processor = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in `plugin_node_spec`.
        let node = unsafe { &plugin(ctx).nodes[index] };
        ExportedProcessor {
            processor: node.provider.create_processor(),
            spec: Arc::clone(&node.spec),
            params: Vec::with_capacity(node.spec.params.len()),
            error: String::new(),
        }
    }));
    let ctx = match processor {
        Ok(processor) => Box::into_raw(Box::new(processor)).cast::<c_void>(),
        Err(_) => ptr::null_mut(),
    };
    FfiProcessor {
        ctx,
        prepare: processor_prepare,
        reset: processor_reset,
        process: processor_process,
        drop: processor_drop,
    }
}

unsafe extern "C" fn plugin_drop(ctx: *mut c_void) {
    // SAFETY: the host calls this once with the ctx it got from `export`.
    let plugin = unsafe { Box::from_raw(ctx.cast::<ExportedPlugin>()) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(plugin)));
}

/// Processor state behind [`FfiProcessor::ctx`].
struct ExportedProcessor {
    processor: Box<dyn NodeProcessor>,
    /// The node type's spec, for the [`ProcessIo`] of each call.
    spec: Arc<NodeSpec>,
    /// Parameter values of the current `process` call, converted from
    /// [`FfiParamValue`]s; sized for the spec's parameters up front, so `process`
    /// doesn't allocate.
    params: Vec<ParamValue>,
    /// Message of the last failed call, borrowed by the returned status.
    error: String,
}

impl ExportedProcessor {
    /// The status for `result` of a call to `what` (`result` is `Err` if it panicked).
    fn status(
        &mut self,
        what: &str,
        result: std::thread::Result<Result<(), PluginError>>,
    ) -> FfiStatus {
        self.error = match result {
            Ok(Ok(())) => return FfiStatus::ok(),
            Ok(Err(err)) => err.message().to_owned(),
            Err(_) => format!("node panicked in {what}"),
        };
        FfiStatus::failed(&self.error)
    }
}

/// # Safety
///
/// `ctx` must be a live, non-null [`FfiProcessor::ctx`].
unsafe fn processor<'a>(ctx: *mut c_void) -> &'a mut ExportedProcessor {
    // SAFETY: forwarded to the caller.
    unsafe { &mut *ctx.cast::<ExportedProcessor>() }
}

unsafe extern "C" fn processor_prepare(
    ctx: *mut c_void,
    sample_rate: f32,
    max_frames: usize,
) -> FfiStatus {
    // SAFETY: the host passes the ctx it got from `create_processor`.
    let state = unsafe { processor(ctx) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        state.processor.prepare(sample_rate, max_frames)
    }));
    state.status("prepare", result)
}

unsafe extern "C" fn processor_reset(ctx: *mut c_void) {
    // SAFETY: as in `processor_prepare`.
    let state = unsafe { processor(ctx) };
    // `reset` can't report errors; a panicking one just leaves the state as is.
    let _ = catch_unwind(AssertUnwindSafe(|| state.processor.reset()));
}

unsafe extern "C" fn processor_process(ctx: *mut c_void, args: FfiProcessArgs) -> FfiStatus {
    // SAFETY: as in `processor_prepare`.
    let state = unsafe { processor(ctx) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        state.params.clear();
        // SAFETY: the host passes arguments that satisfy `ProcessIo::from_raw`
        // (see `FfiProcessor::process`), so every slice in them is valid.
        for &value in unsafe { args.params.as_slice() } {
            state.params.push(ParamValue::try_from(value)?);
        }
        // SAFETY: as above.
        let mut io = unsafe {
            ProcessIo::from_raw(
                args.frames,
                &state.spec,
                &state.params,
                args.inputs.as_slice(),
                args.outputs.as_slice(),
            )
        };
        state.processor.process(&mut io)
    }));
    state.status("process", result)
}

unsafe extern "C" fn processor_drop(ctx: *mut c_void) {
    // SAFETY: the host calls this once with the ctx it got from `create_processor`.
    let processor = unsafe { Box::from_raw(ctx.cast::<ExportedProcessor>()) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(processor)));
}

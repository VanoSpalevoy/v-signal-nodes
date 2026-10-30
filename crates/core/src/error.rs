//! What can go wrong in the node model: [`Error`], matchable by kind.
//!
//! Each failure is its own variant carrying its details, so callers can react to
//! one kind and the messages live only in the `Display` impls. Strings remain only
//! where the text comes from outside: a plugin's own message, the system's loader.

use std::fmt;

use crate::{CompileError, GraphError, NodeKind, SocketDirection, TextKey};

/// An error from the node model or from loading plugins.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// A node spec breaks a rule of [`NodeSpec::validate`](crate::NodeSpec::validate).
    InvalidSpec {
        type_id: String,
        problem: SpecProblem,
    },
    /// No loaded plugin has this node type.
    UnknownNodeType(String),
    /// Another node type already has this id.
    DuplicateNodeType(String),
    /// The node type has no such parameter.
    UnknownParam { type_id: String, param: String },
    /// The parameter's spec doesn't accept the value (wrong kind or out of range).
    ParamRejected { type_id: String, param: String },
    /// A graph rule was broken.
    Graph(GraphError),
    /// A graph can't be compiled.
    Compile(CompileError),
    /// A plugin with this name is already loaded.
    PluginAlreadyLoaded(String),
    /// A dynamic library couldn't be loaded as a plugin; `reason` is the system
    /// loader's or the plugin's message.
    PluginLibrary { path: String, reason: String },
    /// A dynamic library was built against another plugin API version.
    PluginApiMismatch {
        path: String,
        found: u32,
        expected: u32,
    },
    /// A plugin failed; the text is its own message.
    Plugin(String),
}

/// Which rule of [`NodeSpec::validate`](crate::NodeSpec::validate) a spec breaks.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecProblem {
    EmptyTypeId,
    EmptyName,
    /// A socket is in the list of the other direction.
    SocketInWrongList {
        socket: String,
        listed: SocketDirection,
    },
    DuplicateSocket {
        socket: String,
        direction: SocketDirection,
    },
    /// A socket's channel count is outside `1..=MAX_CHANNELS`.
    ChannelCount {
        socket: String,
        channels: u16,
    },
    /// The sockets don't fit the kind: generators need an output, outputs an
    /// input, processors both.
    SocketsDontFitKind {
        kind: NodeKind,
        inputs: usize,
        outputs: usize,
    },
    /// A parameter breaks a rule of [`ParamSpec::validate`](crate::ParamSpec::validate).
    Param {
        param: String,
        problem: ParamProblem,
    },
    DuplicateParam(String),
    /// A translation of a label the spec doesn't have.
    TranslationOfMissingLabel(TextKey),
    DuplicateTranslation {
        language: String,
        key: TextKey,
    },
}

/// Which rule of [`ParamSpec::validate`](crate::ParamSpec::validate) a parameter
/// breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamProblem {
    EmptyId,
    EmptyName,
    /// A number range must be finite with `min <= max`.
    BadRange,
    /// A choice needs options.
    NoOptions,
    DefaultOutOfRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpec { type_id, problem } => write!(f, "node spec '{type_id}': {problem}"),
            Self::UnknownNodeType(type_id) => write!(f, "unknown node type '{type_id}'"),
            Self::DuplicateNodeType(type_id) => {
                write!(f, "node type '{type_id}' is already registered")
            }
            Self::UnknownParam { type_id, param } => {
                write!(f, "{type_id}: no parameter '{param}'")
            }
            Self::ParamRejected { type_id, param } => {
                write!(f, "{type_id}: parameter '{param}' doesn't accept the value")
            }
            Self::Graph(err) => err.fmt(f),
            Self::Compile(err) => err.fmt(f),
            Self::PluginAlreadyLoaded(name) => write!(f, "plugin '{name}' is already loaded"),
            Self::PluginLibrary { path, reason } => write!(f, "{path}: {reason}"),
            Self::PluginApiMismatch {
                path,
                found,
                expected,
            } => write!(
                f,
                "{path}: built for plugin API v{found}, host is v{expected}"
            ),
            Self::Plugin(message) => write!(f, "plugin error: {message}"),
        }
    }
}

impl fmt::Display for SpecProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTypeId => f.write_str("empty type id"),
            Self::EmptyName => f.write_str("empty name"),
            Self::SocketInWrongList { socket, listed } => {
                write!(f, "socket '{socket}' is listed as {listed:?}")
            }
            Self::DuplicateSocket { socket, direction } => {
                write!(f, "duplicate {direction:?} socket '{socket}'")
            }
            Self::ChannelCount { socket, channels } => write!(
                f,
                "socket '{socket}' has {channels} channels, not 1..={}",
                crate::MAX_CHANNELS
            ),
            Self::SocketsDontFitKind {
                kind,
                inputs,
                outputs,
            } => write!(
                f,
                "{kind:?} node with {inputs} input(s) and {outputs} output(s)"
            ),
            Self::Param { param, problem } => write!(f, "parameter '{param}': {problem}"),
            Self::DuplicateParam(param) => write!(f, "duplicate parameter '{param}'"),
            Self::TranslationOfMissingLabel(key) => {
                write!(f, "translation of a missing label {key:?}")
            }
            Self::DuplicateTranslation { language, key } => {
                write!(f, "duplicate '{language}' translation of {key:?}")
            }
        }
    }
}

impl fmt::Display for ParamProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyId => "empty id",
            Self::EmptyName => "empty name",
            Self::BadRange => "range must be finite with min <= max",
            Self::NoOptions => "no options",
            Self::DefaultOutOfRange => "default out of range",
        })
    }
}

impl std::error::Error for Error {}

impl From<GraphError> for Error {
    fn from(err: GraphError) -> Self {
        Self::Graph(err)
    }
}

impl From<CompileError> for Error {
    fn from(err: CompileError) -> Self {
        Self::Compile(err)
    }
}

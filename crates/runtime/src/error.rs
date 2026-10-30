//! What can go wrong while running a plan.

use std::fmt;

use v_signal_nodes_core::NodeId;
use v_signal_nodes_plugin_api::PluginError;

/// Why an [`Engine`](crate::Engine) couldn't be built or a block couldn't be
/// processed.
///
/// TODO(P1-1): `PluginError` carries a `String`, so a failing plugin allocates on
/// the real-time thread; replace with an error code + a message fetched later.
#[derive(Debug)]
pub enum RuntimeError {
    /// The plan has a step for a node no processor was given for.
    MissingProcessor(NodeId),
    /// Creating a node's processor failed; `message` is the host's.
    CreateProcessor { node: NodeId, message: String },
    /// A block longer than the `max_frames` the engine was prepared for.
    TooManyFrames { frames: usize, max_frames: usize },
    /// A node's processor failed in `prepare` or `process`.
    Plugin { node: NodeId, error: PluginError },
    /// A node's processor panicked. The panic was caught and went no further.
    Panicked(NodeId),
    /// Too many messages are waiting for the real-time thread; try again later.
    Busy,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProcessor(node) => write!(f, "no processor for node {node}"),
            Self::CreateProcessor { node, message } => {
                write!(f, "node {node}: can't create its processor: {message}")
            }
            Self::TooManyFrames { frames, max_frames } => {
                write!(
                    f,
                    "block of {frames} frames, prepared for at most {max_frames}"
                )
            }
            Self::Plugin { node, error } => write!(f, "node {node}: {error}"),
            Self::Panicked(node) => write!(f, "node {node} panicked"),
            Self::Busy => f.write_str("the real-time thread isn't keeping up with messages"),
        }
    }
}

impl std::error::Error for RuntimeError {}

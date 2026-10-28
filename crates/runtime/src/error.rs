//! What can go wrong while running a plan.

use std::fmt;

use v_signal_nodes_core::{AppError, NodeId};
use v_signal_nodes_plugin_api::PluginError;

/// Why an [`Engine`](crate::Engine) couldn't be built or a block couldn't be
/// processed.
///
/// TODO(P1-1): `PluginError` carries a `String`, so a failing plugin allocates on
/// the audio thread; replace with an error code + a message fetched later.
#[derive(Debug)]
pub enum RuntimeError {
    /// The plan has a step for a node no processor was given for.
    MissingProcessor(NodeId),
    /// A block longer than the `max_frames` the engine was prepared for.
    TooManyFrames { frames: usize, max_frames: usize },
    /// A node's processor failed in `prepare` or `process`.
    Plugin { node: NodeId, error: PluginError },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProcessor(node) => write!(f, "no processor for node {node}"),
            Self::TooManyFrames { frames, max_frames } => {
                write!(
                    f,
                    "block of {frames} frames, prepared for at most {max_frames}"
                )
            }
            Self::Plugin { node, error } => write!(f, "node {node}: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<RuntimeError> for AppError {
    fn from(err: RuntimeError) -> Self {
        Self::Plugin(err.to_string())
    }
}

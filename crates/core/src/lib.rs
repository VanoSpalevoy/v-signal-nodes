//! The node model and graph of v-signal-nodes, shared by the editor, the plugin
//! API and the runtime.
//!
//! Signals can be of any kind, with a lean towards electrical ones: analog
//! (continuous, modeled as finely sampled streams), sampled (discrete time,
//! continuous value) and digital (discrete time and value). Sound is one kind of
//! sampled signal, not the target. For now samples are real-valued (`f32`);
//! complex (IQ) and bit formats are planned.
//!
//! - [`model`]: node types ([`NodeSpec`]: sockets, parameters, translations),
//!   the [`NodeFactory`] registry and [`Node`] instances.
//! - [`graph`]: the editing model, a [`Graph`] of nodes under generational
//!   [`NodeId`]s and the connection rules (sockets fit, one source per input,
//!   no cycles).
//! - [`plan`]: [`compile`] turns a graph into the flat [`Plan`] executors run.
//!
//! No dependencies except optional `serde` (feature `serde`, for [`ParamValue`]).

pub mod error;
pub mod graph;
pub mod model;
pub mod plan;
pub use crate::error::AppError;
pub use graph::{Graph, GraphError, InputRef, NodeId, OutputRef};
pub use model::{
    ChoiceParam, MAX_CHANNELS, Node, NodeFactory, NodeKind, NodeSocket, NodeSpec, NumberParam,
    ParamKey, ParamKind, ParamSpec, ParamValue, Rgb, SignalRate, SocketDirection, TextKey,
    ToggleParam, Translation,
};
pub use plan::{BufferId, CompileError, Input, Plan, Step, compile};

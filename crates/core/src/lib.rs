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

// #[cfg(test)]

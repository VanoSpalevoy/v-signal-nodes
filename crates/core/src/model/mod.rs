pub mod node;
pub mod node_factory;
pub mod node_socket;
pub mod param;

pub use node::{Node, NodeKind};
pub use node_factory::{NodeFactory, NodeSpec, Rgb, TextKey, Translation};
pub use node_socket::{MAX_CHANNELS, NodeSocket, SignalRate, SocketDirection};
pub use param::{
    ChoiceParam, NumberParam, ParamKey, ParamKind, ParamSpec, ParamValue, ToggleParam,
};

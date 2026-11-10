//! The node model: node types ([`NodeSpec`]) with their sockets, parameters and
//! translations, and [`Node`] instances with their own parameter values.

pub mod node;
pub mod node_socket;
pub mod node_spec;
pub mod param;
pub mod unit;
pub mod window;

pub use node::{Node, NodeKind};
pub use node_socket::{MAX_CHANNELS, NodeSocket, SignalRate, SocketDirection};
pub use node_spec::{NodeSpec, Rgb, TextKey, Translation};
pub use param::{
    ChoiceParam, NumberParam, ParamKey, ParamKind, ParamSpec, ParamValue, ToggleParam,
};
pub use unit::Unit;
pub use window::{MAX_PLOT_BEAMS, MAX_PLOT_POINTS, PlotWindow, WindowContent, WindowSpec};

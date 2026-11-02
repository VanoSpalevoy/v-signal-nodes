//! Windows of a node type: what a node can show besides its controls on the canvas.
//!
//! A plugin declares its windows in the [`NodeSpec`](super::NodeSpec), and the editor
//! gives the node one button per window. What a window holds is described by its
//! [`WindowContent`] and drawn by the editor, so plugins need no UI code and no UI
//! library, also across the C ABI. Declared once as a typed key constant, like
//! parameters:
//!
//! ```
//! use v_signal_nodes_core::{AxisUnit, PlotWindow};
//!
//! const SCREEN: PlotWindow = PlotWindow("screen");
//!
//! let spec = SCREEN.spec("Screen", AxisUnit::Seconds, AxisUnit::Volts);
//! assert_eq!(spec.id, "screen");
//! ```

use crate::model::TextKey;

/// Most points a plot window takes per frame; longer frames are cut to this.
pub const MAX_PLOT_POINTS: usize = 4096;

/// What the numbers on a plot's axis are. An enum, not text, so the editor can
/// translate the unit and pick its prefix (ms, µV).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AxisUnit {
    Seconds,
    Volts,
}

/// What a window of a node shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowContent {
    /// A graph of the frames the node's processor publishes.
    Plot { x: AxisUnit, y: AxisUnit },
}

/// One window of a node type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSpec {
    /// Stable id within the node type.
    pub id: String,
    /// Label (English): the window's title and the tooltip of its button.
    pub name: String,
    pub content: WindowContent,
}

/// Key of a [`Plot`](WindowContent::Plot) window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlotWindow(pub &'static str);

impl PlotWindow {
    /// The spec of this window: a plot with these axes.
    pub fn spec(self, name: impl Into<String>, x: AxisUnit, y: AxisUnit) -> WindowSpec {
        WindowSpec {
            id: self.0.to_owned(),
            name: name.into(),
            content: WindowContent::Plot { x, y },
        }
    }

    /// The [`TextKey`] of the window's name, for translations.
    pub fn name_key(self) -> TextKey {
        TextKey::Window(self.0.to_owned())
    }
}

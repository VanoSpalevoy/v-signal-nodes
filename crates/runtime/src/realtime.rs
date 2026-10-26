//! Real-time processing: an [`Engine`] owned by the audio thread, driven by
//! messages from the editor. Skeleton: only the messages and how the audio thread
//! applies them.
//!
//! The intended shape (TODO P1-4):
//!
//! ```text
//!  editor thread                               audio thread (device callback)
//!  ─────────────                               ──────────────────────────────
//!  compile(&graph) → Plan                      loop per device block:
//!  build processors, Engine::new (prepare)       drain commands (try_recv, no blocking)
//!  ── Command::Replace(engine) ──────────────►   apply(&mut engine, command)
//!  ── Command::SetParam { .. } ─────────────►    engine.process(frames)
//!  ◄── old engine (drop it here) ─────────────   copy the output node's buffer to the device
//! ```
//!
//! Rules on the audio thread: no allocation, no locks, no panics, no dropping of
//! anything big (send the old engine back instead). Still to decide:
//! - the channel: a lock-free SPSC ring both ways (no `std::sync::mpsc`, it may
//!   allocate);
//! - keeping processor state across plans: build the new engine with fresh
//!   processors only for new nodes, and move the old ones over on the audio thread
//!   (needs an `Engine` method that swaps processors by `NodeId` without
//!   allocating);
//! - which node is the output (P1-4 "output nodes"), and summing several;
//! - the audio backend (cpal or similar) lives outside this crate, as a driver
//!   that calls into it; the engine stays testable offline.

use v_signal_nodes_core::{NodeId, ParamValue};

use crate::Engine;

/// A message from the editor to the audio thread.
pub enum Command {
    /// Change one parameter (spec order) of one node from the next block on.
    SetParam {
        node: NodeId,
        index: usize,
        value: ParamValue,
    },
    /// Forget the signal history (playback jumped).
    Reset,
    /// Run a new, already prepared engine (a recompiled graph) from the next block.
    Replace(Box<Engine>),
}

/// Applies `command` to the audio thread's `engine`. Returns the engine it
/// replaced, if any, to be sent back and dropped off the audio thread.
pub fn apply(engine: &mut Box<Engine>, command: Command) -> Option<Box<Engine>> {
    match command {
        Command::SetParam { node, index, value } => {
            engine.set_param(node, index, value);
            None
        }
        Command::Reset => {
            engine.reset();
            None
        }
        Command::Replace(new) => Some(std::mem::replace(engine, new)),
    }
}

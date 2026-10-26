//! Running compiled graphs.
//!
//! The editor compiles its `core::Graph` into a [`Plan`](v_signal_nodes_core::Plan)
//! (`core::compile`) and builds one processor per node (`PluginHost::build_node`).
//! This crate runs them; it depends on `core` and `plugin-api` only, not on the
//! plugin host or an audio backend.
//!
//! | Module         | What                                                          |
//! |----------------|---------------------------------------------------------------|
//! | [`engine`]     | [`Engine`]: runs a plan block by block (offline: [`Engine::run`]) |
//! | [`realtime`]   | messages to an engine on the audio thread (skeleton)           |
//!
//! Both kinds of processing from the plan share the engine: real time calls
//! [`Engine::process`] once per device block (64–512 frames), one pass runs the
//! whole signal in big blocks with [`Engine::run`].

pub mod engine;
pub mod error;
pub mod realtime;

pub use engine::Engine;
pub use error::RuntimeError;

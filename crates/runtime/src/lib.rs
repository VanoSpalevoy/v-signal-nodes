//! Running compiled graphs.
//!
//! The editor compiles its `core::Graph` into a [`Plan`](v_signal_nodes_core::Plan)
//! (`core::compile`) and creates one processor per step (`PluginHost::create_processor`,
//! see [`processors_for`]). This crate runs them; it depends on `core` and
//! `plugin-api` (and a lock-free queue) only, not on the plugin host or a device
//! backend.
//!
//! | Module         | What                                                               |
//! |----------------|--------------------------------------------------------------------|
//! | [`engine`]     | [`Engine`]: runs a plan block by block, one by one or in parallel  |
//! | [`realtime`]   | an engine on a real-time thread, driven by messages                |
//!
//! Two executors of the same plan share the engine:
//! - **real time**: [`realtime::Renderer::render`] on the device's thread, blocks of
//!   64–512 frames, one whole pass over the plan per block. New plans and parameter
//!   changes arrive as messages, without locks or allocation on that thread.
//! - **one pass**: [`Engine::run_parallel`] on any thread, big blocks (about 2^16
//!   frames), every step as soon as the steps it depends on are done.
//!
//! Threads: the thread owning the graph compiles it, creates and prepares
//! processors, and hands them over ready to run; nothing plugin-related is set up on
//! the real-time thread. Processors move, they are never shared: each is used by one
//! thread at a time.

pub mod engine;
pub mod error;
pub mod realtime;

pub use engine::{Engine, processors_for};
pub use error::RuntimeError;

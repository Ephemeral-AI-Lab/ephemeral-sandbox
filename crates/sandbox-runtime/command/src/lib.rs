//! Command execution state and transcript helpers.
//!
//! The namespace-execution engine owns process spawning and PTY I/O. This crate
//! keeps the command-facing handle and transcript windowing used by the
//! operation layer.
//!
//! Mechanism crate, like `sandbox-runtime-overlay` and
//! `sandbox-runtime-namespace-process`. The sandbox
//! runtime this crate backs only ever runs on Linux, so the crate compiles
//! for Linux alone; type-check from other hosts via
//! `cargo check --target x86_64-unknown-linux-gnu`.
#![forbid(unsafe_code)]

mod command_execution;
mod config;
mod contract;
mod transcript_rows;

pub use command_execution::CommandExecution;
pub use config::CommandConfig;
pub use contract::CommandTerminalResult;
pub use transcript_rows::{
    required_transcript_window, transcript_window, CommandStream, CommandTranscriptRow,
    CommandTranscriptWindow,
};

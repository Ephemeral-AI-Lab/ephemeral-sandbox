//! Command process PTY substrate.
//!
//! This crate owns the per-command process/PTY/transcript machinery: spawning
//! the runner child, taking its exit into a policy-free [`process::CommandProcessExit`],
//! cancelling the process group, yield-waiting on output, and retaining the
//! transcript. It carries no workspace policy: who runs on which workspace,
//! and what happens to the upperdir at finalization, is the command-ops tier's
//! concern.
//!
//! Mechanism crate, like `sandbox-runtime-overlay` and
//! `sandbox-runtime-namespace-process`. The sandbox
//! runtime this crate backs only ever runs on Linux, so the crate compiles
//! for Linux alone; type-check from other hosts via
//! `cargo check --target x86_64-unknown-linux-gnu`.
#![forbid(unsafe_code)]

mod cgroup;
mod config;
mod contract;
pub mod process;
pub mod process_group;
mod pty;
mod transcript;
mod transcript_rows;
pub mod yield_wait_loop;

pub use cgroup::CommandCgroupTarget;
pub use config::CommandConfig;
pub use contract::CommandError;
pub use process::{CommandProcess, CommandProcessSpec};
pub use transcript_rows::{
    required_transcript_window, transcript_window, CommandStream, CommandTranscriptRow,
    CommandTranscriptWindow,
};

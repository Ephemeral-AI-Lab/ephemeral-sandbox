//! Command-session PTY substrate.
//!
//! This crate owns the per-session process/PTY/transcript machinery: spawning
//! the runner, reaping the child into a policy-free [`ReapedCommand`], cancelling
//! the process group, and persisting the final response. The caller-keyed
//! workspace-run registry, the publish-vs-discard policy decision, and the
//! completion queue live in the daemon (`eos-daemon`'s `workspace_run` service),
//! which composes this substrate with the overlay/namespace workspace crates.

mod error;
pub mod output;
mod request;
mod response;
#[cfg(any(target_os = "linux", test))]
pub(crate) mod session;
#[cfg(target_os = "linux")]
mod transcript;
#[cfg(any(target_os = "linux", test))]
pub(crate) mod wait;

#[cfg(target_os = "linux")]
pub(crate) mod process;

pub use eos_config::configs::command_session::CommandSessionConfig;
pub use error::CommandSessionError;
pub use output::tail_lines;
pub use request::{
    CancelCommandSession, CollectCompleted, ReadCommandProgress, StartCommandSession, WriteStdin,
};
pub use response::{CollectCompletedResponse, CommandResponse, CommandSessionCompletion};

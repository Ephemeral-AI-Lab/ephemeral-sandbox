//! Workspace-mode-agnostic command-session runtime.

mod error;
mod manager;
pub mod output;
mod registry;
mod request;
mod response;
mod session;
#[cfg(target_os = "linux")]
mod transcript;
#[cfg(any(target_os = "linux", test))]
mod wait;

#[cfg(target_os = "linux")]
pub mod process;

pub mod config {
    pub use eos_config::configs::command_session::*;
}

pub use config::CommandSessionConfig;
pub use error::CommandSessionError;
pub use manager::{CommandSessionManager, SweepReport};
pub use output::tail_lines;
pub use registry::{CommandSessionCompletion, WorkspaceRunKind};
pub use request::{
    CancelCommandSession, CollectCompleted, ReadCommandProgress, StartCommandSession, WriteStdin,
};
pub use response::{CollectCompletedResponse, CommandResponse};

pub type DynCommandWorkspacePolicy =
    Box<dyn eos_workspace_api::CommandWorkspacePolicy + Send + Sync + 'static>;

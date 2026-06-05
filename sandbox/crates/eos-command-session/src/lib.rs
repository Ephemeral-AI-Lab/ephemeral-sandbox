//! Workspace-mode-agnostic command-session runtime.

mod config;
mod error;
mod manager;
pub mod output;
mod registry;
mod request;
mod response;
mod session;
mod wait;

#[cfg(target_os = "linux")]
pub mod process;

pub use config::CommandSessionConfig;
pub use error::CommandSessionError;
pub use manager::{CommandSessionManager, SweepReport};
pub use output::{utf8_consumable_prefix_len, CommandSessionOutput, CommandSessionOutputCursor};
pub use registry::{CommandSessionCompletion, CommandSessionRegistry};
pub use request::{CancelCommandSession, CollectCompleted, StartCommandSession, WriteStdin};
pub use response::{CollectCompletedResponse, CommandResponse};
pub use session::CommandSession;
pub use wait::{wait_for_yield, CommandSessionWaitTarget, WaitOutcome};

pub type DynCommandWorkspacePolicy =
    Box<dyn eos_workspace_api::CommandWorkspacePolicy + Send + Sync + 'static>;

pub(super) mod ephemeral;
pub(super) mod isolated;

use eos_workspace_api::WorkspaceApiError;

/// Wrap any error as the daemon's command-workspace `WorkspaceApiError`, shared
/// by both the ephemeral and isolated command ports.
pub(super) fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("daemon_command_workspace_error", error.to_string())
}

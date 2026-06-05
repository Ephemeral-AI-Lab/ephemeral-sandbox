use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mode::WorkspaceMode;
use crate::response::{ChangedPathKinds, WorkspaceApiError, WorkspaceConflict, WorkspaceTimings};

/// Input needed for a workspace-mode crate to prepare command execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrepareCommandRequest {
    pub agent_id: String,
    pub command_session_id: String,
    pub invocation_id: String,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
}

/// Prepared workspace context returned to daemon-owned command-session control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedCommandWorkspace {
    pub mode: WorkspaceMode,
    pub run_request: Value,
    pub request_path: PathBuf,
    pub output_path: PathBuf,
    pub final_path: PathBuf,
    #[serde(default)]
    pub finalize_context: Value,
}

/// Input needed for mode-specific command workspace finalization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalizeCommandRequest {
    pub finalize_context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_result: Option<Value>,
    #[serde(default)]
    pub command_elapsed_s: f64,
    #[serde(default)]
    pub spool_truncated: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_session_id: Option<String>,
}

/// Normalized command outcome before daemon persistence/parking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCommandOutcome {
    pub mode: WorkspaceMode,
    pub success: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_session_id: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub changed_path_kinds: ChangedPathKinds,
    #[serde(default)]
    pub mutation_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<WorkspaceConflict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<String>,
    #[serde(default)]
    pub timings: WorkspaceTimings,
    #[serde(default)]
    pub metadata: Value,
}

/// Mode-specific command workspace policy. Daemon-owned PTY/process/session
/// registry behavior stays outside this trait.
pub trait CommandWorkspaceOps {
    fn prepare_command_workspace(
        &self,
        request: PrepareCommandRequest,
    ) -> Result<PreparedCommandWorkspace, WorkspaceApiError>;

    fn finalize_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError>;
}

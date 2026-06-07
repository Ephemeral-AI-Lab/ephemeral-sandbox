use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use eos_workspace_api::{WorkspaceCommandOutcome, WorkspaceMode};

use crate::output::tail_lines;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<WorkspaceMode>,
    #[serde(default)]
    pub metadata: Value,
}

impl CommandResponse {
    #[must_use]
    pub fn running(command_session_id: String, stdout: String) -> Self {
        Self {
            status: "running".to_owned(),
            exit_code: None,
            stdout,
            stderr: String::new(),
            command_session_id: Some(command_session_id),
            workspace_mode: None,
            metadata: Value::Null,
        }
    }

    #[must_use]
    pub fn cancelled(stdout: String) -> Self {
        Self {
            status: "cancelled".to_owned(),
            exit_code: None,
            stdout,
            stderr: String::new(),
            command_session_id: None,
            workspace_mode: None,
            metadata: Value::Null,
        }
    }

    #[must_use]
    pub fn error(stderr: impl Into<String>) -> Self {
        Self {
            status: "error".to_owned(),
            exit_code: None,
            stdout: String::new(),
            stderr: stderr.into(),
            command_session_id: None,
            workspace_mode: None,
            metadata: Value::Null,
        }
    }

    #[must_use]
    pub fn from_workspace_outcome(outcome: WorkspaceCommandOutcome) -> Self {
        Self {
            status: outcome.status,
            exit_code: outcome.exit_code,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            command_session_id: outcome.command_session_id,
            workspace_mode: Some(outcome.mode),
            metadata: json!({
                "success": outcome.success,
                "changed_paths": outcome.changed_paths,
                "changed_path_kinds": outcome.changed_path_kinds,
                "mutation_source": outcome.mutation_source,
                "conflict": outcome.conflict,
                "conflict_reason": outcome.conflict_reason,
                "timings": outcome.timings,
                "metadata": outcome.metadata,
            }),
        }
    }

    #[must_use]
    pub fn with_last_lines(mut self, last_n_lines: usize) -> Self {
        self.stdout = tail_lines(&self.stdout, last_n_lines);
        self
    }

    #[must_use]
    pub fn to_wire_value(&self) -> Value {
        let mut response = json!({
            "status": self.status,
            "exit_code": self.exit_code,
            "output": {
                "stdout": self.stdout,
                "stderr": self.stderr,
            },
        });
        if let Some(command_session_id) = self.command_session_id.as_ref() {
            response["command_session_id"] = json!(command_session_id);
        }
        let Some(mode) = self.workspace_mode else {
            return response;
        };
        let mode = mode.as_str();
        response["success"] = self
            .metadata
            .get("success")
            .cloned()
            .unwrap_or_else(|| json!(self.status == "ok"));
        response["workspace"] = json!(mode);
        response["workspace_mode"] = json!(mode);
        response["stdout"] = json!(self.stdout);
        response["stderr"] = json!(self.stderr);
        response["conflict"] = self
            .metadata
            .get("conflict")
            .cloned()
            .unwrap_or(Value::Null);
        response["conflict_reason"] = self
            .metadata
            .get("conflict_reason")
            .cloned()
            .unwrap_or(Value::Null);
        response["changed_paths"] = self
            .metadata
            .get("changed_paths")
            .cloned()
            .unwrap_or_else(|| json!([]));
        response["changed_path_kinds"] = self
            .metadata
            .get("changed_path_kinds")
            .cloned()
            .unwrap_or_else(|| json!({}));
        response["mutation_source"] = self
            .metadata
            .get("mutation_source")
            .cloned()
            .unwrap_or_else(|| json!(""));
        response["error"] = Value::Null;
        response["timings"] = self
            .metadata
            .get("timings")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(metadata) = self.metadata.get("metadata").and_then(Value::as_object) {
            for (key, value) in metadata {
                response[key] = value.clone();
            }
        }
        response
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectCompletedResponse {
    pub success: bool,
    pub completions: Vec<crate::CommandSessionCompletion>,
}

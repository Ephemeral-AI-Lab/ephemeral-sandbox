use thiserror::Error;

pub const MAX_ID_LENGTH: usize = 256;
pub const MAX_KIND_LENGTH: usize = 64;
const MAX_STATUS_LENGTH: usize = 64;
pub const MAX_OPERATION_LENGTH: usize = 128;
const MAX_METHOD_LENGTH: usize = 256;
const MAX_ERROR_KIND_LENGTH: usize = 128;
pub const MAX_ERROR_MESSAGE_LENGTH: usize = 4096;
pub const MAX_SNAPSHOT_STATE_LENGTH: usize = 64;
pub const MAX_PATH_LENGTH: usize = 4096;

#[derive(Debug, Error)]
pub enum RecordValidationError {
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds {max_len} bytes")]
    TooLong { field: &'static str, max_len: usize },
    #[error("span trace_id {span_trace_id} does not match trace_id {trace_id}")]
    SpanTraceMismatch {
        trace_id: String,
        span_trace_id: String,
    },
    #[error("{field} sandbox_id {actual} does not match {expected}")]
    SandboxMismatch {
        field: &'static str,
        expected: String,
        actual: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceRecord {
    pub trace_id: String,
    pub kind: String,
    pub status: String,
    pub sandbox_id: String,
    pub operation: String,
    pub request_id: Option<String>,
    pub origin_request_id: Option<String>,
    pub workspace_id: Option<String>,
    pub command_session_id: Option<String>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

impl TraceRecord {
    pub(crate) fn validate(&self) -> Result<(), RecordValidationError> {
        validate_required("trace_id", &self.trace_id, MAX_ID_LENGTH)?;
        validate_required("kind", &self.kind, MAX_KIND_LENGTH)?;
        validate_required("status", &self.status, MAX_STATUS_LENGTH)?;
        validate_required("sandbox_id", &self.sandbox_id, MAX_ID_LENGTH)?;
        validate_required("operation", &self.operation, MAX_OPERATION_LENGTH)?;
        validate_optional("request_id", self.request_id.as_deref(), MAX_ID_LENGTH)?;
        validate_optional(
            "origin_request_id",
            self.origin_request_id.as_deref(),
            MAX_ID_LENGTH,
        )?;
        validate_optional("workspace_id", self.workspace_id.as_deref(), MAX_ID_LENGTH)?;
        validate_optional(
            "command_session_id",
            self.command_session_id.as_deref(),
            MAX_ID_LENGTH,
        )?;
        validate_optional(
            "error_kind",
            self.error_kind.as_deref(),
            MAX_ERROR_KIND_LENGTH,
        )?;
        validate_optional(
            "error_message",
            self.error_message.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpanRecord {
    pub span_id: String,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub method_name: String,
    pub call_index: i64,
    pub status: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

impl SpanRecord {
    pub(crate) fn validate_for_trace(&self, trace_id: &str) -> Result<(), RecordValidationError> {
        validate_required("span_id", &self.span_id, MAX_ID_LENGTH)?;
        validate_required("trace_id", &self.trace_id, MAX_ID_LENGTH)?;
        validate_optional(
            "parent_span_id",
            self.parent_span_id.as_deref(),
            MAX_ID_LENGTH,
        )?;
        validate_required("method_name", &self.method_name, MAX_METHOD_LENGTH)?;
        validate_required("status", &self.status, MAX_STATUS_LENGTH)?;
        validate_optional(
            "error_kind",
            self.error_kind.as_deref(),
            MAX_ERROR_KIND_LENGTH,
        )?;
        validate_optional(
            "error_message",
            self.error_message.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;

        if self.trace_id != trace_id {
            return Err(RecordValidationError::SpanTraceMismatch {
                trace_id: trace_id.to_owned(),
                span_trace_id: self.trace_id.clone(),
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxSnapshotRecord {
    pub sandbox_id: String,
    pub state: String,
    pub workspace_root: Option<String>,
    pub daemon_runtime_dir: Option<String>,
    pub socket_path: Option<String>,
    pub pid_path: Option<String>,
    pub daemon_pid: Option<i64>,
    pub sampled_at_unix_ms: i64,
    pub error_message: Option<String>,
}

impl SandboxSnapshotRecord {
    pub(crate) fn validate(&self) -> Result<(), RecordValidationError> {
        validate_required("sandbox_id", &self.sandbox_id, MAX_ID_LENGTH)?;
        validate_required("state", &self.state, MAX_SNAPSHOT_STATE_LENGTH)?;
        validate_optional(
            "workspace_root",
            self.workspace_root.as_deref(),
            MAX_PATH_LENGTH,
        )?;
        validate_optional(
            "daemon_runtime_dir",
            self.daemon_runtime_dir.as_deref(),
            MAX_PATH_LENGTH,
        )?;
        validate_optional("socket_path", self.socket_path.as_deref(), MAX_PATH_LENGTH)?;
        validate_optional("pid_path", self.pid_path.as_deref(), MAX_PATH_LENGTH)?;
        validate_optional(
            "error_message",
            self.error_message.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshotRecord {
    pub sandbox_id: String,
    pub workspace_id: String,
    pub state: String,
    pub remount_state: Option<String>,
    pub profile: Option<String>,
    pub workspace_root: Option<String>,
    pub upperdir: Option<String>,
    pub workdir: Option<String>,
    pub namespace_fd_count: Option<i64>,
    pub base_manifest_version: Option<i64>,
    pub base_root_hash: Option<String>,
    pub layer_count: Option<i64>,
    pub sampled_at_unix_ms: i64,
    pub error_message: Option<String>,
}

impl WorkspaceSnapshotRecord {
    pub(crate) fn validate_for_sandbox(
        &self,
        sandbox_id: &str,
    ) -> Result<(), RecordValidationError> {
        validate_sandbox_match("workspace_snapshot", sandbox_id, &self.sandbox_id)?;
        validate_required("workspace_id", &self.workspace_id, MAX_ID_LENGTH)?;
        validate_required("state", &self.state, MAX_SNAPSHOT_STATE_LENGTH)?;
        validate_optional(
            "remount_state",
            self.remount_state.as_deref(),
            MAX_SNAPSHOT_STATE_LENGTH,
        )?;
        validate_optional("profile", self.profile.as_deref(), MAX_KIND_LENGTH)?;
        validate_optional(
            "workspace_root",
            self.workspace_root.as_deref(),
            MAX_PATH_LENGTH,
        )?;
        validate_optional("upperdir", self.upperdir.as_deref(), MAX_PATH_LENGTH)?;
        validate_optional("workdir", self.workdir.as_deref(), MAX_PATH_LENGTH)?;
        validate_optional(
            "base_root_hash",
            self.base_root_hash.as_deref(),
            MAX_ID_LENGTH,
        )?;
        validate_optional(
            "error_message",
            self.error_message.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceExecutionSnapshotRecord {
    pub sandbox_id: String,
    pub namespace_execution_id: String,
    pub workspace_session_id: String,
    pub operation: String,
    pub lifecycle_state: String,
    pub sampled_at_unix_ms: i64,
    pub error_message: Option<String>,
}

impl NamespaceExecutionSnapshotRecord {
    pub(crate) fn validate_for_sandbox(
        &self,
        sandbox_id: &str,
    ) -> Result<(), RecordValidationError> {
        validate_sandbox_match("namespace_execution_snapshot", sandbox_id, &self.sandbox_id)?;
        validate_required(
            "namespace_execution_id",
            &self.namespace_execution_id,
            MAX_ID_LENGTH,
        )?;
        validate_required(
            "workspace_session_id",
            &self.workspace_session_id,
            MAX_ID_LENGTH,
        )?;
        validate_required("operation", &self.operation, MAX_OPERATION_LENGTH)?;
        validate_required(
            "lifecycle_state",
            &self.lifecycle_state,
            MAX_SNAPSHOT_STATE_LENGTH,
        )?;
        validate_optional(
            "error_message",
            self.error_message.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamespaceExecutionTraceRecord {
    pub trace_id: String,
    pub sandbox_id: String,
    pub namespace_execution_id: String,
    pub workspace_session_id: String,
    pub operation: String,
    pub request_id: Option<String>,
    pub status: String,
    pub exit_code: Option<i64>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub duration_ms: f64,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

impl NamespaceExecutionTraceRecord {
    pub(crate) fn validate(&self) -> Result<(), RecordValidationError> {
        validate_required("trace_id", &self.trace_id, MAX_ID_LENGTH)?;
        validate_required("sandbox_id", &self.sandbox_id, MAX_ID_LENGTH)?;
        validate_required(
            "namespace_execution_id",
            &self.namespace_execution_id,
            MAX_ID_LENGTH,
        )?;
        validate_required(
            "workspace_session_id",
            &self.workspace_session_id,
            MAX_ID_LENGTH,
        )?;
        validate_required("operation", &self.operation, MAX_OPERATION_LENGTH)?;
        validate_optional("request_id", self.request_id.as_deref(), MAX_ID_LENGTH)?;
        validate_required("status", &self.status, MAX_STATUS_LENGTH)?;
        validate_optional(
            "error_kind",
            self.error_kind.as_deref(),
            MAX_ERROR_KIND_LENGTH,
        )?;
        validate_optional(
            "error_message",
            self.error_message.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceSampleRecord {
    pub sample_id: String,
    pub sandbox_id: String,
    pub workspace_id: Option<String>,
    pub sampled_at_unix_ms: i64,
    pub cgroup_path: Option<String>,
    pub cgroup_available: bool,
    pub cgroup_error: Option<String>,
    pub cpu_usage_usec: Option<i64>,
    pub memory_current_bytes: Option<i64>,
    pub memory_max_bytes: Option<i64>,
    pub memory_max_unlimited: Option<bool>,
    pub disk_upperdir_bytes: Option<i64>,
    pub disk_file_count: Option<i64>,
    pub disk_dir_count: Option<i64>,
    pub disk_symlink_count: Option<i64>,
    pub disk_truncated: Option<bool>,
    pub disk_read_error_count: Option<i64>,
    pub disk_first_error_path: Option<String>,
}

impl ResourceSampleRecord {
    pub(crate) fn validate(&self) -> Result<(), RecordValidationError> {
        validate_required("sample_id", &self.sample_id, MAX_ID_LENGTH)?;
        validate_required("sandbox_id", &self.sandbox_id, MAX_ID_LENGTH)?;
        validate_optional("workspace_id", self.workspace_id.as_deref(), MAX_ID_LENGTH)?;
        validate_optional("cgroup_path", self.cgroup_path.as_deref(), MAX_PATH_LENGTH)?;
        validate_optional(
            "cgroup_error",
            self.cgroup_error.as_deref(),
            MAX_ERROR_MESSAGE_LENGTH,
        )?;
        validate_optional(
            "disk_first_error_path",
            self.disk_first_error_path.as_deref(),
            MAX_PATH_LENGTH,
        )?;
        Ok(())
    }
}

fn validate_sandbox_match(
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), RecordValidationError> {
    validate_required("sandbox_id", expected, MAX_ID_LENGTH)?;
    validate_required("sandbox_id", actual, MAX_ID_LENGTH)?;
    if expected == actual {
        Ok(())
    } else {
        Err(RecordValidationError::SandboxMismatch {
            field,
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        })
    }
}

fn validate_required(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), RecordValidationError> {
    if value.is_empty() {
        return Err(RecordValidationError::Empty { field });
    }

    validate_len(field, value, max_len)
}

fn validate_optional(
    field: &'static str,
    value: Option<&str>,
    max_len: usize,
) -> Result<(), RecordValidationError> {
    if let Some(value) = value {
        validate_len(field, value, max_len)?;
    }

    Ok(())
}

fn validate_len(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), RecordValidationError> {
    if value.len() > max_len {
        return Err(RecordValidationError::TooLong { field, max_len });
    }

    Ok(())
}

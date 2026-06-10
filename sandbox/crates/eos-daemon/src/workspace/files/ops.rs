//! Workspace file op router.

use std::path::PathBuf;

use eos_config::configs::daemon::{MAX_FILE_BYTES, MAX_READ_BYTES};
use eos_file_ops::{
    edit_file, read_file, write_file, DirectBackend, EditFileOutcome, EditFileRequest,
    FileOpsError, ReadFileOutcome, ReadFileRequest, SearchReplaceEdit, WorkspaceConflict,
    WorkspaceMode, WriteFileOutcome, WriteFileRequest,
};
#[cfg(target_os = "linux")]
use eos_file_ops::IsolatedBackend;
use serde_json::{json, Value};

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
use crate::request_args::{require_raw_string, require_string};

/// `api.v1.read_file` — shared public read op, routed by active workspace mode.
pub(crate) fn op_read_file(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let request = read_request(args, context)?;
    #[cfg(target_os = "linux")]
    if let Some(binding) = crate::workspace::isolated::command_handle_for_args(args) {
        let outcome = read_file(&isolated_backend(&binding), request).map_err(workspace_error)?;
        crate::workspace::isolated::touch_isolated(&binding.caller_id);
        return Ok(read_response(outcome));
    }
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let outcome = read_file(&DirectBackend::new(root), request).map_err(workspace_error)?;
    Ok(read_response(outcome))
}

/// `api.v1.write_file` — shared public write op, routed by active workspace mode.
pub(crate) fn op_write_file(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let request = write_request(args, context)?;
    #[cfg(target_os = "linux")]
    if let Some(binding) = crate::workspace::isolated::command_handle_for_args(args) {
        let outcome = write_file(&isolated_backend(&binding), request).map_err(workspace_error)?;
        crate::workspace::isolated::touch_isolated(&binding.caller_id);
        return Ok(write_response(outcome));
    }
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let outcome = write_file(&DirectBackend::new(root), request).map_err(workspace_error)?;
    Ok(write_response(outcome))
}

/// `api.v1.edit_file` — shared public edit op, routed by active workspace mode.
pub(crate) fn op_edit_file(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let request = edit_request(args)?;
    #[cfg(target_os = "linux")]
    if let Some(binding) = crate::workspace::isolated::command_handle_for_args(args) {
        let outcome = edit_file(&isolated_backend(&binding), request).map_err(workspace_error)?;
        crate::workspace::isolated::touch_isolated(&binding.caller_id);
        return Ok(edit_response(outcome));
    }
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let outcome = edit_file(&DirectBackend::new(root), request).map_err(workspace_error)?;
    Ok(edit_response(outcome))
}

fn read_request(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<ReadFileRequest, DaemonError> {
    Ok(ReadFileRequest {
        path: require_string(args, "path")?,
        max_read_bytes: context
            .file_limits()
            .map_or(MAX_READ_BYTES, |limits| limits.max_read_bytes),
    })
}

fn write_request(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<WriteFileRequest, DaemonError> {
    Ok(WriteFileRequest {
        path: require_string(args, "path")?,
        content: require_raw_string(args, "content")?.into_bytes(),
        overwrite: args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        max_file_bytes: context
            .file_limits()
            .map_or(MAX_FILE_BYTES, |limits| limits.max_write_bytes),
    })
}

fn edit_request(args: &Value) -> Result<EditFileRequest, DaemonError> {
    let edits = args
        .get("edits")
        .and_then(Value::as_array)
        .ok_or_else(|| DaemonError::InvalidEnvelope("edits must be a list".to_owned()))?;
    let mut parsed = Vec::with_capacity(edits.len());
    for raw in edits {
        let edit: SearchReplaceEdit = serde_json::from_value(raw.clone())
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
        if edit.old_text.is_empty() {
            return Err(DaemonError::InvalidEnvelope(
                "edit anchor old_text must be non-empty".to_owned(),
            ));
        }
        parsed.push(edit);
    }
    Ok(EditFileRequest {
        path: require_string(args, "path")?,
        edits: parsed,
    })
}

fn read_response(outcome: ReadFileOutcome) -> Value {
    json!({
        "success": outcome.success,
        "workspace": mode(outcome.mode),
        "workspace_mode": mode(outcome.mode),
        "content": outcome.content,
        "exists": outcome.exists,
        "encoding": outcome.encoding,
        "timings": outcome.timings,
    })
}

fn write_response(outcome: WriteFileOutcome) -> Value {
    GuardedWireResponse {
        workspace_mode: outcome.mode,
        success: outcome.success,
        published: outcome.published,
        status: outcome.status,
        conflict: outcome.conflict,
        conflict_reason: outcome.conflict_reason,
        changed_paths: outcome.changed_paths,
        changed_path_kinds: outcome.changed_path_kinds,
        mutation_source: outcome.mutation_source,
        timings: outcome.timings,
        applied_edits: None,
    }
    .into_json()
}

fn edit_response(outcome: EditFileOutcome) -> Value {
    GuardedWireResponse {
        workspace_mode: outcome.mode,
        success: outcome.success,
        published: outcome.published,
        status: outcome.status,
        conflict: outcome.conflict,
        conflict_reason: outcome.conflict_reason,
        changed_paths: outcome.changed_paths,
        changed_path_kinds: outcome.changed_path_kinds,
        mutation_source: outcome.mutation_source,
        timings: outcome.timings,
        applied_edits: Some(outcome.applied_edits),
    }
    .into_json()
}

struct GuardedWireResponse {
    workspace_mode: WorkspaceMode,
    success: bool,
    published: bool,
    status: String,
    conflict: Option<WorkspaceConflict>,
    conflict_reason: Option<String>,
    changed_paths: Vec<String>,
    changed_path_kinds: std::collections::BTreeMap<String, String>,
    mutation_source: String,
    timings: eos_file_ops::WorkspaceTimings,
    applied_edits: Option<i64>,
}

impl GuardedWireResponse {
    fn into_json(self) -> Value {
        let mut response = json!({
            "success": self.success,
            "published": self.published,
            "workspace": mode(self.workspace_mode),
            "workspace_mode": mode(self.workspace_mode),
            "changed_paths": self.changed_paths,
            "changed_path_kinds": self.changed_path_kinds,
            "mutation_source": self.mutation_source,
            "status": self.status,
            "conflict": self.conflict.map(conflict_value),
            "conflict_reason": self.conflict_reason,
            "error": null,
            "timings": self.timings,
        });
        if let Some(applied_edits) = self.applied_edits {
            response["applied_edits"] = json!(applied_edits);
        }
        response
    }
}

fn conflict_value(conflict: WorkspaceConflict) -> Value {
    json!({
        "reason": conflict.reason,
        "conflict_file": conflict.conflict_file,
        "message": conflict.message,
    })
}

fn mode(mode: WorkspaceMode) -> &'static str {
    mode.as_str()
}

fn workspace_error(error: FileOpsError) -> DaemonError {
    DaemonError::InvalidEnvelope(error.to_string())
}

/// Build the isolated file backend from the caller's open binding.
#[cfg(target_os = "linux")]
fn isolated_backend(
    binding: &crate::workspace::isolated::IsolatedCommandHandle,
) -> IsolatedBackend {
    IsolatedBackend {
        layer_stack_root: binding.layer_stack_root.clone(),
        workspace_root: binding.workspace_root.clone(),
        upperdir: binding.upperdir.clone(),
        layer_paths: binding.layer_paths.clone(),
        manifest_version: binding.manifest_version,
        manifest_root_hash: binding.manifest_root_hash.clone(),
    }
}

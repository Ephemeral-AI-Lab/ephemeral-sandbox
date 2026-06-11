//! Workspace file op router.

use eos_config::configs::daemon::{MAX_FILE_BYTES, MAX_READ_BYTES};
use eos_file_ops::{
    EditFileOutcome, EditFileRequest, MutationOutcome, ReadFileOutcome, ReadFileRequest,
    SearchReplaceEdit, WorkspaceConflict, WriteFileRequest,
};
use eos_runtime::routing::file_op::{self, FileOpContext, FileOpError, FileRoute};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::request_args::{optional_path, require_raw_string, require_string};
use crate::response::GuardedResponse;
use crate::DispatchContext;

/// `api.v1.read_file` — shared public read op, routed by active workspace mode.
pub(crate) fn op_read_file(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let request = read_request(args, context)?;
    let caller_id = super::caller_id_or_default(args);
    let routed = file_op::read_file(file_context(args, context, &caller_id), request)
        .map_err(file_op_error)?;
    let mut outcome = routed.outcome;
    if let FileRoute::Direct { layer_stack_root } = routed.route {
        enrich_direct_timings(&layer_stack_root, &mut outcome.timings, 0);
    }
    Ok(read_response(outcome))
}

/// `api.v1.write_file` — shared public write op, routed by active workspace mode.
pub(crate) fn op_write_file(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let request = write_request(args, context)?;
    let caller_id = super::caller_id_or_default(args);
    let routed = file_op::write_file(file_context(args, context, &caller_id), request)
        .map_err(file_op_error)?;
    let mut outcome = routed.outcome;
    if let FileRoute::Direct { layer_stack_root } = routed.route {
        enrich_direct_timings(
            &layer_stack_root,
            &mut outcome.timings,
            outcome.changed_paths.len(),
        );
    }
    Ok(mutation_response(outcome, None))
}

/// `api.v1.edit_file` — shared public edit op, routed by active workspace mode.
pub(crate) fn op_edit_file(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let request = edit_request(args)?;
    let caller_id = super::caller_id_or_default(args);
    let routed = file_op::edit_file(file_context(args, context, &caller_id), request)
        .map_err(file_op_error)?;
    let EditFileOutcome {
        mut mutation,
        applied_edits,
    } = routed.outcome;
    if let FileRoute::Direct { layer_stack_root } = routed.route {
        enrich_direct_timings(
            &layer_stack_root,
            &mut mutation.timings,
            mutation.changed_paths.len(),
        );
    }
    Ok(mutation_response(mutation, Some(applied_edits)))
}

fn file_context<'a, 'ctx: 'a>(
    args: &Value,
    context: DispatchContext<'ctx>,
    caller_id: &'a str,
) -> FileOpContext<'a> {
    FileOpContext {
        workspace: context.services().map(|services| &services.workspace),
        caller_id,
        layer_stack_root: optional_path(args, "layer_stack_root"),
    }
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
        "workspace": outcome.workspace_kind,
        "content": outcome.content,
        "exists": outcome.exists,
        "encoding": outcome.encoding,
        "timings": outcome.timings,
    })
}

fn mutation_response(outcome: MutationOutcome, applied_edits: Option<i64>) -> Value {
    GuardedResponse {
        success: outcome.success,
        published: Some(outcome.published),
        workspace: outcome.workspace_kind,
        changed_paths: json!(outcome.changed_paths),
        changed_path_kinds: json!(outcome.changed_path_kinds),
        mutation_source: outcome.mutation_source,
        status: outcome.status,
        conflict: outcome.conflict.map(conflict_value),
        conflict_reason: outcome.conflict_reason,
        timings: json!(outcome.timings),
        applied_edits,
    }
    .into_json()
}

fn conflict_value(conflict: WorkspaceConflict) -> Value {
    json!({
        "reason": conflict.reason,
        "conflict_file": conflict.conflict_file,
        "message": conflict.message,
    })
}

/// Splice the daemon's latest-state resource sample (manifest depth, tree-key
/// seeds, cgroup/process gauges) into a direct file-op response — the wire
/// layer's enrichment, so the file-ops crate stays free of process telemetry.
fn enrich_direct_timings(
    root: &std::path::Path,
    timings: &mut eos_file_ops::WorkspaceTimings,
    changed_path_count: usize,
) {
    if let Ok(manifest) = eos_layerstack::service::active_manifest(root) {
        for (key, value) in crate::response::resource_timings(&manifest, changed_path_count) {
            timings.entry(key).or_insert(value);
        }
    }
}

fn file_op_error(error: FileOpError) -> DaemonError {
    match error {
        FileOpError::MissingLayerStackRoot => {
            DaemonError::InvalidEnvelope("layer_stack_root is required".to_owned())
        }
        FileOpError::File(error) => DaemonError::InvalidEnvelope(error.to_string()),
    }
}

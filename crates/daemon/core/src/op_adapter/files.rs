//! Workspace file op router.

use std::path::PathBuf;

use config::configs::daemon::{MAX_FILE_BYTES, MAX_READ_BYTES};
use layerstack::CommitOptions;
use operation::file::contract::{EditFileInput, ReadFileInput, WriteFileInput};
use operation::file::{
    edit_file as edit_with_backend, read_file as read_with_backend,
    write_file as write_with_backend, DirectBackend, EditFileOutcome, EditFileRequest,
    FileOpsError, IsolatedBackend, ReadFileOutcome, ReadFileRequest, WorkspaceTimings,
    WriteFileOutcome, WriteFileRequest,
};
use serde_json::{json, Map, Value};
use thiserror::Error;
use workspace::IsolatedWorkspaceBinding;

use crate::error::DaemonError;
use crate::runtime::workspace_runtime::{WorkspaceFileRouteContext, WorkspaceRouteTraceFacts};
use crate::{DispatchContext, WorkspaceRuntime};

use super::{ok_envelope, to_wire_value};

struct RoutedFileOutcome<T> {
    route: WorkspaceFileRouteContext,
    outcome: T,
}

struct FileOpContext<'a> {
    workspace: Option<&'a WorkspaceRuntime>,
    caller_id: &'a str,
    layer_stack_root: Option<PathBuf>,
    commit_options: CommitOptions,
}

#[derive(Debug, Error)]
enum FileOpError {
    #[error("layer_stack_root is required")]
    MissingLayerStackRoot,
    #[error(transparent)]
    Workspace(#[from] workspace::WorkspaceError),
    #[error(transparent)]
    File(#[from] FileOpsError),
}

impl FileOpError {
    fn from_workspace(error: workspace::WorkspaceError) -> Self {
        if is_missing_layer_stack_root(&error) {
            Self::MissingLayerStackRoot
        } else {
            Self::Workspace(error)
        }
    }
}

/// `sandbox.file.read` — shared public read op, routed by active workspace mode.
pub(crate) fn op_read_file(
    input: ReadFileInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let max_read_bytes = context
        .file_limits()
        .map_or(MAX_READ_BYTES, |limits| limits.max_read_bytes);
    record_file_event(
        &context,
        "read_started",
        json!({"path": input.path, "max_read_bytes": max_read_bytes}),
    );
    let request = ReadFileRequest {
        path: input.path,
        max_read_bytes,
    };
    let caller_id = input.caller.to_string();
    let routed = route_read_file(
        file_context(input.layer_stack_root, &context, &caller_id),
        request,
    )
    .map_err(file_op_error)?;
    record_file_route(&context, &routed.route);
    let mut outcome = routed.outcome;
    if let Some(layer_stack_root) = routed.route.direct_layer_stack_root() {
        enrich_direct_timings(layer_stack_root, &mut outcome.timings, 0);
        record_resource_stats_from_timings(&context, "after", &outcome.timings);
    }
    record_read_finished(&context, &outcome);
    Ok(ok_envelope(read_response(outcome)))
}

/// `sandbox.file.write` — shared public write op, routed by active workspace mode.
pub(crate) fn op_write_file(
    input: WriteFileInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let max_file_bytes = context
        .file_limits()
        .map_or(MAX_FILE_BYTES, |limits| limits.max_write_bytes);
    record_file_event(
        &context,
        "mutation_started",
        json!({
            "kind": "write",
            "path": input.path,
            "content_bytes": input.content.len(),
            "overwrite": input.overwrite,
            "max_file_bytes": max_file_bytes,
        }),
    );
    let request = WriteFileRequest {
        path: input.path,
        content: input.content.into_bytes(),
        overwrite: input.overwrite,
        max_file_bytes,
    };
    let caller_id = input.caller.to_string();
    let routed = route_write_file(
        file_context(input.layer_stack_root, &context, &caller_id),
        request,
    )
    .map_err(file_op_error)?;
    record_file_route(&context, &routed.route);
    let mut outcome = routed.outcome;
    if let Some(layer_stack_root) = routed.route.direct_layer_stack_root() {
        enrich_direct_timings(
            layer_stack_root,
            &mut outcome.core.timings,
            outcome.core.changed_paths.len(),
        );
        record_resource_stats_from_timings(&context, "after", &outcome.core.timings);
    }
    record_occ_trace_events(&context, &outcome.trace_events);
    record_mutation_finished(&context, "write_applied", &outcome);
    Ok(ok_envelope(mutation_response(outcome)))
}

/// `sandbox.file.edit` — shared public edit op, routed by active workspace mode.
pub(crate) fn op_edit_file(
    input: EditFileInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let max_read_bytes = context
        .file_limits()
        .map_or(MAX_READ_BYTES, |limits| limits.max_read_bytes);
    let max_file_bytes = context
        .file_limits()
        .map_or(MAX_FILE_BYTES, |limits| limits.max_write_bytes);
    record_file_event(
        &context,
        "mutation_started",
        json!({
            "kind": "edit",
            "path": input.path,
            "edit_count": input.edits.len(),
            "max_read_bytes": max_read_bytes,
            "max_file_bytes": max_file_bytes,
        }),
    );
    let request = EditFileRequest {
        path: input.path,
        edits: input.edits,
        max_read_bytes,
        max_file_bytes,
    };
    let caller_id = input.caller.to_string();
    let routed = route_edit_file(
        file_context(input.layer_stack_root, &context, &caller_id),
        request,
    )
    .map_err(file_op_error)?;
    record_file_route(&context, &routed.route);
    let mut mutation = routed.outcome;
    if let Some(layer_stack_root) = routed.route.direct_layer_stack_root() {
        enrich_direct_timings(
            layer_stack_root,
            &mut mutation.core.timings,
            mutation.core.changed_paths.len(),
        );
        record_resource_stats_from_timings(&context, "after", &mutation.core.timings);
    }
    record_occ_trace_events(&context, &mutation.trace_events);
    record_mutation_finished(&context, "edit_applied", &mutation);
    Ok(ok_envelope(mutation_response(mutation)))
}

fn file_context<'a, 'ctx: 'a>(
    layer_stack_root: Option<PathBuf>,
    context: &'a DispatchContext<'ctx>,
    caller_id: &'a str,
) -> FileOpContext<'a> {
    FileOpContext {
        workspace: context.services().map(|services| &services.workspace),
        caller_id,
        layer_stack_root,
        commit_options: context
            .services()
            .map_or_else(CommitOptions::default, |services| services.commit_options),
    }
}

fn record_file_route(context: &DispatchContext<'_>, route: &WorkspaceFileRouteContext) {
    record_route_selected(context, &route.trace_facts());
}

fn record_route_selected(context: &DispatchContext<'_>, facts: &WorkspaceRouteTraceFacts) {
    let details = if let Some(layer_stack_root) = &facts.layer_stack_root {
        json!({
            "kind": facts.kind,
            "reason": facts.reason,
            "layer_stack_root": layer_stack_root,
        })
    } else {
        json!({
            "kind": facts.kind,
            "reason": facts.reason,
        })
    };
    context.record_trace_event("workspace.route", "route_selected", details);
}

fn record_file_event(context: &DispatchContext<'_>, name: &'static str, details: Value) {
    context.record_trace_event("file", name, details);
}

fn record_occ_trace_events(context: &DispatchContext<'_>, events: &[layerstack::OccTraceEvent]) {
    for event in events {
        context.record_trace_event(event.module, event.name, event.details.clone());
    }
}

fn record_read_finished(context: &DispatchContext<'_>, outcome: &ReadFileOutcome) {
    record_file_event(
        context,
        "read_finished",
        json!({
            "workspace": outcome.workspace_kind.as_str(),
            "success": outcome.success,
            "exists": outcome.exists,
            "encoding": outcome.encoding,
            "content_bytes": outcome.content.len(),
        }),
    );
}

fn record_mutation_finished(
    context: &DispatchContext<'_>,
    name: &'static str,
    outcome: &WriteFileOutcome,
) {
    record_file_event(
        context,
        name,
        json!({
            "workspace": outcome.workspace_kind.as_str(),
            "success": outcome.core.success,
            "published": outcome.published,
            "status": outcome.status.as_str(),
            "changed_paths": outcome.core.changed_paths,
            "changed_path_count": outcome.core.changed_paths.len(),
            "conflict_reason": outcome.core.conflict_reason,
            "applied_edits": outcome.applied_edits,
        }),
    );
}

fn record_resource_stats_from_timings(
    context: &DispatchContext<'_>,
    phase: &'static str,
    timings: &WorkspaceTimings,
) {
    let mut cpu = Map::new();
    let mut memory = Map::new();
    let mut io = Map::new();
    let mut psi = Map::new();
    let mut process = Map::new();
    for (key, value) in timings {
        if let Some(name) = key.strip_prefix("resource.cgroup.cpu_") {
            cpu.insert(name.to_owned(), value.clone());
        } else if let Some(name) = key.strip_prefix("resource.cgroup.memory_") {
            memory.insert(name.to_owned(), value.clone());
        } else if let Some(name) = key.strip_prefix("resource.cgroup.io_") {
            io.insert(name.to_owned(), value.clone());
        } else if let Some(name) = key.strip_prefix("resource.cgroup.psi_") {
            psi.insert(name.to_owned(), value.clone());
        } else if let Some(name) = key.strip_prefix("resource.process.") {
            process.insert(name.to_owned(), value.clone());
        }
    }
    let cgroup_available =
        !(cpu.is_empty() && memory.is_empty() && io.is_empty() && psi.is_empty());
    let process_available = !process.is_empty();
    let sampler_duration_us = timings
        .get("resource.sampler.cgroup_process_duration_us")
        .cloned()
        .unwrap_or(Value::Null);
    context.record_trace_event(
        "resource",
        "resource_stats",
        json!({
            "meta": {
                "stats_kind": "cgroup_process",
                "phase": phase,
                "source": "daemon.response_timings",
                "source_available": cgroup_available || process_available,
                "read_error": (!(cgroup_available || process_available)).then_some("resource timings unavailable on this platform or request path"),
                "sampler_duration_us": sampler_duration_us,
                "inflight_requests": context
                    .invocation_registry()
                    .map_or(0, crate::invocation_registry::InFlightRegistry::inflight_count),
            },
            "cgroup": {
                "source_available": cgroup_available,
                "cpu": cpu,
                "memory": memory,
                "io": io,
                "psi": psi,
            },
            "process": {
                "source_available": process_available,
                "gauges": process,
            },
        }),
    );
}

fn route_read_file(
    context: FileOpContext<'_>,
    request: ReadFileRequest,
) -> Result<RoutedFileOutcome<ReadFileOutcome>, FileOpError> {
    let direct_request = request.clone();
    route_file_op(
        context,
        |binding| read_with_backend(&isolated_backend(binding), request),
        |root| read_with_backend(&DirectBackend::new(root), direct_request),
    )
}

fn route_write_file(
    context: FileOpContext<'_>,
    request: WriteFileRequest,
) -> Result<RoutedFileOutcome<WriteFileOutcome>, FileOpError> {
    let direct_request = request.clone();
    let commit_options = context.commit_options;
    route_file_op(
        context,
        |binding| write_with_backend(&isolated_backend(binding), request),
        |root| {
            write_with_backend(
                &DirectBackend::with_commit_options(root, commit_options),
                direct_request,
            )
        },
    )
}

fn route_edit_file(
    context: FileOpContext<'_>,
    request: EditFileRequest,
) -> Result<RoutedFileOutcome<EditFileOutcome>, FileOpError> {
    let direct_request = request.clone();
    let commit_options = context.commit_options;
    route_file_op(
        context,
        |binding| edit_with_backend(&isolated_backend(binding), request),
        |root| {
            edit_with_backend(
                &DirectBackend::with_commit_options(root, commit_options),
                direct_request,
            )
        },
    )
}

fn route_file_op<T>(
    context: FileOpContext<'_>,
    isolated: impl FnOnce(&IsolatedWorkspaceBinding) -> Result<T, FileOpsError>,
    direct: impl FnOnce(PathBuf) -> Result<T, FileOpsError>,
) -> Result<RoutedFileOutcome<T>, FileOpError> {
    let route = match context.workspace {
        Some(workspace) => workspace
            .route_file_context(context.caller_id, context.layer_stack_root.as_deref())
            .map_err(FileOpError::from_workspace)?,
        None => WorkspaceRuntime::direct_file_context(context.layer_stack_root.as_deref())
            .map_err(FileOpError::from_workspace)?,
    };
    let outcome = match &route {
        WorkspaceFileRouteContext::Isolated { binding } => {
            let outcome = isolated(binding)?;
            if let Some(workspace) = context.workspace {
                workspace.complete_file_route(&route);
            }
            outcome
        }
        WorkspaceFileRouteContext::Direct { layer_stack_root } => direct(layer_stack_root.clone())?,
    };
    Ok(RoutedFileOutcome { route, outcome })
}

fn isolated_backend(binding: &IsolatedWorkspaceBinding) -> IsolatedBackend {
    IsolatedBackend {
        layer_stack_root: binding.layer_stack_root.clone(),
        workspace_root: binding.workspace_root.clone(),
        upperdir: binding.upperdir.clone(),
        layer_paths: binding.layer_paths.clone(),
        manifest_version: binding.manifest_version,
        manifest_root_hash: binding.manifest_root_hash.clone(),
    }
}

fn read_response(outcome: ReadFileOutcome) -> Value {
    json!({
        "workspace": outcome.workspace_kind,
        "success": outcome.success,
        "content": outcome.content,
        "exists": outcome.exists,
        "encoding": outcome.encoding,
    })
}

fn mutation_response(outcome: WriteFileOutcome) -> Value {
    let mut value = to_wire_value(outcome);
    if let Some(object) = value.as_object_mut() {
        object.remove("timings");
    }
    value
}

/// Splice the daemon's latest-state resource sample (manifest depth, tree-key
/// seeds, cgroup/process gauges) into a direct file-op response — the wire
/// layer's enrichment, so the file-ops crate stays free of process telemetry.
fn enrich_direct_timings(
    root: &std::path::Path,
    timings: &mut operation::file::WorkspaceTimings,
    changed_path_count: usize,
) {
    if let Ok(manifest) = layerstack::service::active_manifest(root) {
        for (key, value) in crate::response::resource_timings(&manifest, changed_path_count) {
            timings.entry(key).or_insert(value);
        }
    }
}

fn file_op_error(error: FileOpError) -> DaemonError {
    match error {
        FileOpError::MissingLayerStackRoot => {
            DaemonError::InvalidRequest("layer_stack_root is required".to_owned())
        }
        FileOpError::Workspace(error) => DaemonError::InvalidRequest(error.to_string()),
        FileOpError::File(error) => DaemonError::InvalidRequest(error.to_string()),
    }
}

fn is_missing_layer_stack_root(error: &workspace::WorkspaceError) -> bool {
    matches!(
        error,
        workspace::WorkspaceError::InvalidRequest { field, message }
            if *field == "layer_stack_root" && message == "layer_stack_root is required"
    )
}

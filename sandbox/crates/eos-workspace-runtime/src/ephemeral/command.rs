//! Publishable ephemeral command-workspace lifecycle as free functions.
//!
//! The daemon's ephemeral workspace run owns the snapshot lease + run dirs and
//! calls these directly: [`prepare_ephemeral_command`] lays out the fresh overlay
//! and runner request, and [`finalize_ephemeral_command`] captures the upperdir
//! and publishes it through the daemon's [`WorkspacePublisherPort`] on COMPLETE.
//! Discard (cancel) never calls finalize, so a cancelled command never reaches
//! the OCC merge — the run just removes the dirs and releases the lease.

use std::path::PathBuf;

use eos_workspace_contract::{
    u64_to_f64_saturating, usize_to_f64_saturating, ChangedPathKinds, FinalizeCommandRequest,
    PrepareCommandRequest, PreparedCommandWorkspace, WorkspaceApiError, WorkspaceCommandOutcome,
    WorkspaceConflict, WorkspaceMode, WorkspaceTimings,
};
use serde_json::{json, Value};

use crate::ephemeral::{
    finalize_publishable_workspace, CallerId, EphemeralDirAllocator, EphemeralRunDirs,
    SnapshotLease, EphemeralWorkspace, EphemeralWorkspaceError, FinalizeRequest, InvocationId,
    PathChange, PathChangeKind, PublishOutcome, TreeResourceStats, WorkspacePublisherPort,
    WorkspaceRoot,
};

/// Daemon-supplied facts needed to prepare a publishable command workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralCommandPrepareContext {
    pub layer_stack_root: PathBuf,
    pub workspace_root: PathBuf,
    pub writable_root: PathBuf,
    pub session_dir: PathBuf,
    pub final_path: PathBuf,
}

/// A prepared ephemeral command workspace: the runner-facing handles plus the
/// owned overlay state (snapshot lease + run dirs) the daemon run keeps until the
/// command settles.
pub struct PreparedEphemeralCommand {
    pub prepared: PreparedCommandWorkspace,
    pub workspace: EphemeralWorkspace,
}

/// Lay out a fresh overlay for one command session and build its runner request.
///
/// The caller has already acquired `snapshot`; on failure here the caller is
/// responsible for releasing that lease (this function never publishes or
/// releases). The returned `workspace` is the state the run owns until it
/// finalizes (publish) or discards (cancel).
///
/// # Errors
///
/// Returns [`WorkspaceApiError`] when the session dir, metadata, or run dirs
/// cannot be created.
pub fn prepare_ephemeral_command(
    context: EphemeralCommandPrepareContext,
    snapshot: SnapshotLease,
    request: PrepareCommandRequest,
) -> Result<PreparedEphemeralCommand, WorkspaceApiError> {
    let PrepareCommandRequest {
        caller_id,
        command_session_id,
        invocation_id,
        cmd,
        timeout_seconds,
    } = request;
    std::fs::create_dir_all(&context.session_dir).map_err(prepare_error)?;
    std::fs::write(
        context.session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "command_session_id": &command_session_id,
            "caller_id": &caller_id,
            "invocation_id": &invocation_id,
            "workspace": "ephemeral",
            "command": &cmd,
            "status": "running",
        }))
        .map_err(prepare_error)?,
    )
    .map_err(prepare_error)?;
    let mut dirs = EphemeralDirAllocator::new(context.writable_root)
        .allocate("sandbox-overlay", &InvocationId(invocation_id.clone()))
        .map_err(prepare_workspace_error)?;
    dirs.output_path = dirs.run_dir.join("command-runner-result.json");
    let request_path = dirs.run_dir.join("command-runner-request.json");
    dirs.request_path = Some(request_path.clone());
    dirs.final_path = context.final_path.clone();

    let run_request = json!({
        "mode": "fresh_ns",
        "tool_call": {
            "invocation_id": &invocation_id,
            "caller_id": &caller_id,
            "verb": "exec_command",
            "intent": "write_allowed",
            "args": {
                "command": &cmd,
                "cwd": ".",
            },
            "background": false,
        },
        "workspace_root": context.workspace_root.clone(),
        "layer_paths": snapshot.layer_paths.clone(),
        "upperdir": dirs.upperdir.clone(),
        "workdir": dirs.workdir.clone(),
        "ns_fds": null,
        "cgroup_path": null,
        "timeout_seconds": timeout_seconds,
    });

    let prepared = PreparedCommandWorkspace {
        run_request,
        request_path,
        output_path: dirs.output_path.clone(),
        final_path: context.final_path,
        session_dir: context.session_dir.clone(),
        transcript_path: context.session_dir.join("transcript.log"),
    };
    let workspace = EphemeralWorkspace {
        layer_stack_root: WorkspaceRoot(context.layer_stack_root),
        workspace_root: context.workspace_root,
        caller_id: CallerId(caller_id),
        invocation_id: InvocationId(invocation_id),
        snapshot,
        dirs,
    };
    Ok(PreparedEphemeralCommand {
        prepared,
        workspace,
    })
}

/// Capture the command's upperdir, publish it through `publisher`, and shape the
/// command outcome. This is the COMPLETE branch only — the run calls it when the
/// command settled normally; cancel discards instead and never reaches here.
///
/// # Errors
///
/// Returns [`WorkspaceApiError`] when capture or publish-result parsing fails.
pub fn finalize_ephemeral_command(
    publisher: &impl WorkspacePublisherPort,
    workspace: EphemeralWorkspace,
    base_timings: WorkspaceTimings,
    request: FinalizeCommandRequest,
) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
    let run_dir = workspace.dirs.run_dir.clone();
    let finalize = finalize_publishable_workspace(publisher, FinalizeRequest { workspace })
        .map_err(finalize_error)?;
    let files = publish_files(&finalize.publish)?;
    let path_kinds = path_changes_to_wire(&finalize.capture.path_kinds);
    let changed_path_kinds = path_kinds.into_iter().collect::<ChangedPathKinds>();
    let first_conflict = files.iter().find(|file| !status_is_success(&file.status));
    let command_success = request.command_succeeded();
    let publish_success = files.iter().all(|file| status_is_success(&file.status));
    let mut timings = base_timings;
    timings.insert(
        "resource.command_exec.changed_path_count".to_owned(),
        json!(usize_to_f64_saturating(changed_path_kinds.len())),
    );
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &finalize.capture.stats,
    );
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.run_dir",
        &TreeResourceStats::collect(&run_dir),
    );
    for (key, value) in &finalize.publish.timings {
        timings.insert(key.clone(), value.clone());
    }
    let occ_s = timing_as_f64(&timings, "occ.commit.total_s")
        .or(finalize.timings.publish_s)
        .unwrap_or_default();
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(finalize.capture.capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(occ_s));
    timings.insert(
        "command_exec.total_s".to_owned(),
        json!(request.command_elapsed_s),
    );
    timings.insert(
        "api.exec_command.dispatch_total_s".to_owned(),
        json!(request.command_elapsed_s),
    );

    Ok(WorkspaceCommandOutcome {
        mode: WorkspaceMode::Ephemeral,
        success: command_success && publish_success,
        status: request.status,
        exit_code: request.exit_code,
        stdout: request.stdout,
        stderr: request.stderr,
        command_session_id: request.command_session_id,
        changed_paths: files
            .iter()
            .filter(|file| status_is_published(&file.status))
            .map(|file| file.path.clone())
            .collect(),
        changed_path_kinds,
        mutation_source: "overlay_capture".to_owned(),
        conflict: first_conflict.map(conflict_from_file),
        conflict_reason: first_conflict.map(|file| conflict_message(file).to_owned()),
        timings,
        metadata: Value::Null,
    })
}

/// Discard a prepared overlay WITHOUT publishing: remove the run dirs. The
/// snapshot lease is released by the daemon run (it owns the LayerStack handle).
/// Best-effort — a stale dir is reclaimed by the orphan reaper.
pub fn discard_ephemeral_command(dirs: &EphemeralRunDirs) {
    let _ = std::fs::remove_dir_all(&dirs.run_dir);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishFile {
    path: String,
    status: String,
    message: String,
}

fn prepare_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_prepare_failed", error.to_string())
}

fn prepare_workspace_error(error: EphemeralWorkspaceError) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_prepare_failed", error.to_string())
}

fn finalize_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_finalize_failed", error.to_string())
}

// Hand-parses the publisher port's untyped `PublishOutcome.raw` JSON. It depends
// on the `{ "files": [ { "path": str, "status": str, "message"?: str } ] }`
// contract the publisher emits: `path`/`status` are required strings, `message`
// is optional. When `files` is absent, it falls back to `published_paths`.
fn publish_files(outcome: &PublishOutcome) -> Result<Vec<PublishFile>, WorkspaceApiError> {
    let Some(files) = outcome.raw.get("files").and_then(Value::as_array) else {
        return Ok(outcome
            .published_paths
            .iter()
            .map(|path| PublishFile {
                path: path.clone(),
                status: "committed".to_owned(),
                message: String::new(),
            })
            .collect());
    };
    files
        .iter()
        .map(|file| {
            let object = file
                .as_object()
                .ok_or_else(|| finalize_error("publish file result must be an object"))?;
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| finalize_error("publish file result missing path"))?;
            let status = object
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| finalize_error("publish file result missing status"))?;
            Ok(PublishFile {
                path: path.to_owned(),
                status: status.to_owned(),
                message: object
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect()
}

fn path_changes_to_wire(path_changes: &[PathChange]) -> Vec<(String, String)> {
    path_changes
        .iter()
        .map(|change| {
            (
                change.path.clone(),
                path_change_kind_wire(change.kind).to_owned(),
            )
        })
        .collect()
}

const fn path_change_kind_wire(kind: PathChangeKind) -> &'static str {
    match kind {
        PathChangeKind::Write => "write",
        PathChangeKind::Delete => "delete",
        PathChangeKind::Symlink => "symlink",
        PathChangeKind::OpaqueDir => "opaque_dir",
    }
}

fn status_is_published(status: &str) -> bool {
    matches!(status, "accepted" | "committed")
}

fn status_is_success(status: &str) -> bool {
    matches!(status, "accepted" | "committed" | "dropped")
}

fn conflict_from_file(file: &PublishFile) -> WorkspaceConflict {
    WorkspaceConflict::path(&file.status, &file.path, conflict_message(file))
}

fn conflict_message(file: &PublishFile) -> &str {
    if file.message.is_empty() {
        file.status.as_str()
    } else {
        file.message.as_str()
    }
}

fn timing_as_f64(timings: &WorkspaceTimings, key: &str) -> Option<f64> {
    timings.get(key).and_then(Value::as_f64)
}

/// Emit `<prefix>_tree_*` resource counters for a captured scratch tree. Used
/// for both the overlay upperdir (the published delta) and the run dir (scratch
/// metadata); both stay proportional to per-operation writes, never to the
/// shared lowerdir workspace size.
fn insert_tree_resource_timings(
    timings: &mut WorkspaceTimings,
    prefix: &str,
    stats: &TreeResourceStats,
) {
    let file_entries = stats.files.saturating_add(stats.symlinks);
    let entry_count = file_entries.saturating_add(stats.dirs);
    insert_resource_timing(
        timings,
        &format!("{prefix}_tree_exists"),
        entry_count.min(1),
    );
    insert_resource_timing(timings, &format!("{prefix}_tree_bytes"), stats.bytes);
    insert_resource_timing(timings, &format!("{prefix}_tree_file_count"), file_entries);
    insert_resource_timing(timings, &format!("{prefix}_tree_dir_count"), stats.dirs);
    insert_resource_timing(timings, &format!("{prefix}_tree_entry_count"), entry_count);
    insert_resource_timing(timings, &format!("{prefix}_tree_truncated"), 0);
}

fn insert_resource_timing(timings: &mut WorkspaceTimings, key: &str, value: u64) {
    timings.insert(key.to_owned(), json!(u64_to_f64_saturating(value)));
}

#[cfg(test)]
#[path = "../../tests/ephemeral/command_unit.rs"]
mod tests;

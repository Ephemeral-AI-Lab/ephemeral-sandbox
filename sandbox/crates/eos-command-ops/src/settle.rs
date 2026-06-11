use std::path::Path;

use eos_command_session::CommandResponse;
use eos_ephemeral_workspace::{path_changes_to_wire, EphemeralWorkspace, TreeResourceStats};
use eos_layerstack::service::Snapshot;
use eos_layerstack::{service, FileResult};
use serde_json::{json, Value};

use crate::outcome::{
    ChangedPathKinds, FinalizeCommandRequest, WorkspaceApiError, WorkspaceConflict,
    WorkspaceTimings,
};
use crate::CommandBinding;

pub(crate) fn settle_ephemeral(
    root: &Path,
    snapshot: &Snapshot,
    workspace: &EphemeralWorkspace,
    request: FinalizeCommandRequest,
) -> Result<CommandResponse, WorkspaceApiError> {
    let mut timings = base_timings(root)?;
    let captured = workspace.capture().map_err(finalize_error)?;
    let publish_start = std::time::Instant::now();
    let changeset = service::publish_capture(
        root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured.changes,
    )
    .map_err(finalize_error)?;
    let publish_s = publish_start.elapsed().as_secs_f64();

    let path_kinds = path_changes_to_wire(&captured.path_kinds);
    let changed_path_kinds = path_kinds.into_iter().collect::<ChangedPathKinds>();
    let first_conflict = changeset.first_conflict();
    let command_success = request.command_succeeded();
    let publish_success = changeset.success();

    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &captured.stats,
    );
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.run_dir",
        &TreeResourceStats::collect(&workspace.dirs().run_dir),
    );
    for (key, value) in &changeset.timings {
        timings.insert(key.clone(), json!(value));
    }
    let occ_s = changeset
        .timings
        .get("occ.commit.total_s")
        .copied()
        .unwrap_or(publish_s);
    insert_command_timings(
        &mut timings,
        changed_path_kinds.len(),
        captured.capture_s,
        occ_s,
        request.command_elapsed_s,
        false,
    );

    Ok(command_response(
        "ephemeral",
        request,
        command_success && publish_success,
        changeset.published_paths(),
        changed_path_kinds,
        "overlay_capture",
        first_conflict.map(conflict_from_file),
        first_conflict.map(|file| conflict_message(file).to_owned()),
        timings,
        Value::Null,
    ))
}

pub(crate) fn settle_isolated(
    binding: &CommandBinding,
    request: FinalizeCommandRequest,
) -> Result<CommandResponse, WorkspaceApiError> {
    let mut timings = base_timings(&binding.layer_stack_root)?;
    let capture_start = std::time::Instant::now();
    let changes = eos_overlay::capture_upperdir(&binding.upperdir)
        .map_err(|err| finalize_error(format!("capture isolated upperdir: {err}")))?;
    let capture_s = capture_start.elapsed().as_secs_f64();
    let changed_path_kinds = changes
        .iter()
        .map(|change| {
            let kind = match change {
                eos_overlay::LayerChange::Write { .. } => "write",
                eos_overlay::LayerChange::Delete { .. } => "delete",
                eos_overlay::LayerChange::Symlink { .. } => "symlink",
                eos_overlay::LayerChange::OpaqueDir { .. } => "opaque_dir",
            };
            (change.path().as_str().to_owned(), kind.to_owned())
        })
        .collect::<ChangedPathKinds>();
    let changed_paths: Vec<String> = changed_path_kinds.keys().cloned().collect();
    merge_runner_timings(&mut timings, request.runner_result.as_ref());
    let command_success = request.command_succeeded();
    insert_command_timings(
        &mut timings,
        changed_paths.len(),
        capture_s,
        0.0,
        request.command_elapsed_s,
        true,
    );
    let metadata = json!({
            "isolated_workspace": {
                "caller_id": binding.caller_id,
                "workspace_handle_id": binding.workspace_handle_id,
                "manifest_version": binding.manifest_version,
                "manifest_root_hash": binding.manifest_root_hash,
                "published": false,
            },
            "warnings": [],
    });
    let mut response = command_response(
        "isolated",
        request,
        command_success,
        changed_paths,
        changed_path_kinds,
        "isolated_workspace",
        None,
        None,
        timings,
        metadata,
    );
    response.exit_code = Some(response.exit_code.unwrap_or(1));
    Ok(response)
}

pub(crate) fn discarded_response(
    workspace_kind: &'static str,
    request: FinalizeCommandRequest,
) -> CommandResponse {
    command_response(
        workspace_kind,
        request,
        false,
        Vec::new(),
        ChangedPathKinds::default(),
        "",
        None,
        None,
        WorkspaceTimings::default(),
        Value::Null,
    )
}

fn command_response(
    workspace_kind: &'static str,
    request: FinalizeCommandRequest,
    success: bool,
    changed_paths: Vec<String>,
    changed_path_kinds: ChangedPathKinds,
    mutation_source: &'static str,
    conflict: Option<WorkspaceConflict>,
    conflict_reason: Option<String>,
    timings: WorkspaceTimings,
    metadata: Value,
) -> CommandResponse {
    CommandResponse {
        status: request.status,
        exit_code: request.exit_code,
        stdout: request.stdout,
        stderr: request.stderr,
        command_session_id: request.command_session_id,
        workspace: Some(workspace_kind.to_owned()),
        metadata: json!({
            "success": success,
            "changed_paths": changed_paths,
            "changed_path_kinds": changed_path_kinds,
            "mutation_source": mutation_source,
            "conflict": conflict,
            "conflict_reason": conflict_reason,
            "timings": timings,
            "metadata": metadata,
        }),
    }
}

fn insert_command_timings(
    timings: &mut WorkspaceTimings,
    changed_path_count: usize,
    capture_s: f64,
    occ_s: f64,
    elapsed_s: f64,
    include_api_total: bool,
) {
    for (key, value) in [
        (
            "resource.command_exec.changed_path_count",
            usize_to_f64_saturating(changed_path_count),
        ),
        ("command_exec.capture_upperdir_s", capture_s),
        ("command_exec.occ_apply_s", occ_s),
        ("command_exec.total_s", elapsed_s),
        ("api.exec_command.dispatch_total_s", elapsed_s),
    ] {
        timings.insert(key.to_owned(), json!(value));
    }
    if include_api_total {
        timings.insert("api.exec_command.total_s".to_owned(), json!(elapsed_s));
    }
}

fn base_timings(root: &Path) -> Result<WorkspaceTimings, WorkspaceApiError> {
    let manifest = service::active_manifest(root).map_err(|error| {
        WorkspaceApiError::new("daemon_command_workspace_error", error.to_string())
    })?;
    let mut timings = WorkspaceTimings::new();
    timings.insert(
        "resource.command_exec.changed_path_count".to_owned(),
        json!(0.0),
    );
    timings.insert(
        "resource.layer_stack.manifest_depth".to_owned(),
        json!(usize_to_f64_saturating(manifest.depth())),
    );
    timings.insert(
        "resource.layer_stack.manifest_path_count".to_owned(),
        json!(usize_to_f64_saturating(manifest.depth())),
    );
    for tree in ["run_dir", "workspace", "upperdir"] {
        for suffix in [
            "exists",
            "bytes",
            "file_count",
            "dir_count",
            "entry_count",
            "truncated",
        ] {
            timings.insert(
                format!("resource.command_exec.{tree}_tree_{suffix}"),
                json!(0.0),
            );
        }
    }
    insert_cgroup_resource_timings(&mut timings);
    insert_process_resource_timings(&mut timings);
    Ok(timings)
}

fn insert_cgroup_resource_timings(timings: &mut WorkspaceTimings) {
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/cpu.stat") {
        for line in raw.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else {
                continue;
            };
            let Some(value) = parts.next().and_then(|raw| raw.parse::<f64>().ok()) else {
                continue;
            };
            timings.insert(format!("resource.cgroup.cpu_{name}"), json!(value));
        }
    }

    for (path, key) in [
        (
            "/sys/fs/cgroup/memory.current",
            "resource.cgroup.memory_current_bytes",
        ),
        (
            "/sys/fs/cgroup/memory.peak",
            "resource.cgroup.memory_peak_bytes",
        ),
    ] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(value) = raw.trim().parse::<f64>() {
                timings.insert(key.to_owned(), json!(value));
            }
        }
    }

    let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/io.stat") else {
        return;
    };
    let mut totals = std::collections::BTreeMap::<&str, f64>::from([
        ("rbytes", 0.0),
        ("wbytes", 0.0),
        ("rios", 0.0),
        ("wios", 0.0),
        ("dbytes", 0.0),
        ("dios", 0.0),
    ]);
    for line in raw.lines() {
        for token in line.split_whitespace().skip(1) {
            let Some((name, raw_value)) = token.split_once('=') else {
                continue;
            };
            let Some(total) = totals.get_mut(name) else {
                continue;
            };
            if let Ok(value) = raw_value.parse::<f64>() {
                *total += value;
            }
        }
    }
    for (name, value) in totals {
        timings.insert(format!("resource.cgroup.io_{name}"), json!(value));
    }
}

fn insert_process_resource_timings(timings: &mut WorkspaceTimings) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return;
    };
    for line in status.lines() {
        let key = match line.split(':').next() {
            Some("VmRSS") => "resource.process.rss_bytes",
            Some("VmHWM") => "resource.process.max_rss_bytes",
            _ => continue,
        };
        if let Some(kib) = line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<f64>().ok())
        {
            timings.insert(key.to_owned(), json!(kib * 1024.0));
        }
    }
}

fn merge_runner_timings(timings: &mut WorkspaceTimings, runner_result: Option<&Value>) {
    let Some(runner_timings) = runner_result
        .and_then(|result| result.get("timings"))
        .and_then(Value::as_object)
    else {
        return;
    };
    for (key, value) in runner_timings {
        timings.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

fn conflict_from_file(file: &FileResult) -> WorkspaceConflict {
    let reason = file.status.wire_str();
    WorkspaceConflict::path(reason, file.path.as_str(), file.conflict_message(reason))
}

fn conflict_message(file: &FileResult) -> &str {
    file.conflict_message(file.status.wire_str())
}

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

fn u64_to_f64_saturating(value: u64) -> f64 {
    const U32_FACTOR: f64 = 4_294_967_296.0;
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high).mul_add(U32_FACTOR, f64::from(low))
}

fn usize_to_f64_saturating(value: usize) -> f64 {
    u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

fn finalize_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("command_finalize_failed", error.to_string())
}

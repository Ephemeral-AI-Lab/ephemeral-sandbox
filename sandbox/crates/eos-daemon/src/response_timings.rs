//! Shared daemon response shaping and resource timing helpers.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;
use std::time::Instant;

use eos_occ::{ChangesetResult, FileResult, OccStatus};
use eos_protocol::{LayerChange, Manifest};
use eos_runner::RunResult;
use serde_json::{json, Value};

const TREE_RESOURCE_ENTRY_LIMIT: usize = 2_000;

#[derive(Clone, Copy, Debug)]
pub(crate) struct TreeResourceStats {
    exists: f64,
    bytes: f64,
    file_count: f64,
    dir_count: f64,
    entry_count: f64,
    truncated: f64,
}

impl TreeResourceStats {
    fn missing() -> Self {
        Self {
            exists: 0.0,
            bytes: 0.0,
            file_count: 0.0,
            dir_count: 0.0,
            entry_count: 0.0,
            truncated: 0.0,
        }
    }

    pub(crate) fn collect(path: &Path) -> Self {
        let Ok(root_metadata) = fs::symlink_metadata(path) else {
            return Self::missing();
        };
        let root_is_dir = root_metadata.is_dir();
        let mut stats = Self {
            exists: 1.0,
            bytes: allocated_bytes(&root_metadata),
            file_count: if root_is_dir { 0.0 } else { 1.0 },
            dir_count: if root_is_dir { 1.0 } else { 0.0 },
            entry_count: 1.0,
            truncated: 0.0,
        };
        if !root_is_dir {
            return stats;
        }

        let mut queue = VecDeque::from([path.to_path_buf()]);
        while let Some(current) = queue.pop_front() {
            let Ok(entries) = fs::read_dir(current) else {
                continue;
            };
            for entry in entries.flatten() {
                if stats.entry_count >= usize_to_f64_saturating(TREE_RESOURCE_ENTRY_LIMIT) {
                    stats.truncated = 1.0;
                    break;
                }
                let entry_path = entry.path();
                let Ok(metadata) = fs::symlink_metadata(&entry_path) else {
                    continue;
                };
                let is_dir = metadata.is_dir();
                stats.entry_count += 1.0;
                stats.bytes += allocated_bytes(&metadata);
                if is_dir {
                    stats.dir_count += 1.0;
                    queue.push_back(entry_path);
                } else {
                    stats.file_count += 1.0;
                }
            }
            if stats.truncated > 0.0 {
                break;
            }
        }
        if !queue.is_empty() {
            stats.truncated = 1.0;
        }
        stats
    }
}

pub(crate) fn attach_runner_shell_fields(response: &mut Value, runner: &RunResult) {
    response["exit_code"] = runner
        .tool_result
        .get("exit_code")
        .cloned()
        .unwrap_or_else(|| json!(runner.exit_code));
    response["stdout"] = runner
        .tool_result
        .get("stdout")
        .cloned()
        .unwrap_or_else(|| json!(""));
    response["stderr"] = runner
        .tool_result
        .get("stderr")
        .cloned()
        .unwrap_or_else(|| json!(""));
    response["warnings"] = runner
        .tool_result
        .get("warnings")
        .cloned()
        .unwrap_or_else(|| json!([]));
}

pub(crate) fn merge_runner_timings(
    timings: &mut serde_json::Map<String, Value>,
    runner: &RunResult,
) {
    if let Some(runner_timings) = runner.tool_result.get("timings").and_then(Value::as_object) {
        for (key, value) in runner_timings {
            timings.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    if let Some(value) = timings.get("workspace.mount_s").cloned() {
        timings
            .entry("command_exec.mount_workspace_s".to_owned())
            .or_insert(value);
    }
    if let Some(value) = timings.get("workspace.tool_s").cloned() {
        timings
            .entry("command_exec.run_command_s".to_owned())
            .or_insert(value);
    }
}

pub(crate) const fn layer_change_kind(change: &LayerChange) -> &'static str {
    match change {
        LayerChange::Write { .. } => "write",
        LayerChange::Delete { .. } => "delete",
        LayerChange::Symlink { .. } => "symlink",
        LayerChange::OpaqueDir { .. } => "opaque_dir",
    }
}

pub(crate) fn guarded_changeset_response(
    verb: &str,
    result: &ChangesetResult,
    mut timings: serde_json::Map<String, Value>,
    total_start: Instant,
    applied_edits: Option<i64>,
) -> Value {
    for (key, value) in &result.timings {
        timings.insert(key.clone(), json!(value));
    }
    timings.insert(
        format!("api.{verb}.total_s"),
        json!(total_start.elapsed().as_secs_f64()),
    );
    let changed_paths: Vec<String> = result
        .files
        .iter()
        .filter(|file| file.status.is_published())
        .map(|file| file.path.as_str().to_owned())
        .collect();
    let mut changed_path_kinds = serde_json::Map::new();
    for path in &changed_paths {
        changed_path_kinds.insert(path.to_owned(), json!("write"));
    }
    let conflict = first_conflict(result);
    let mut response = json!({
        "success": result.success(),
        "workspace": "ephemeral",
        "changed_paths": changed_paths,
        "changed_path_kinds": Value::Object(changed_path_kinds),
        "mutation_source": mutation_source(verb),
        "status": conflict
            .as_ref()
            .map_or("committed", |file| occ_status_wire(file.status)),
        "conflict": conflict.as_ref().map(|file| json!({
            "reason": occ_status_wire(file.status),
            "conflict_file": file.path.as_str(),
            "message": if file.message.is_empty() { occ_status_wire(file.status) } else { file.message.as_str() },
        })),
        "conflict_reason": conflict.as_ref().map(|file| {
            if file.message.is_empty() { occ_status_wire(file.status) } else { file.message.as_str() }
        }),
        "error": null,
        "timings": Value::Object(timings),
    });
    if let Some(count) = applied_edits {
        response["applied_edits"] = json!(count);
    }
    response
}

pub(crate) fn published_file_count(result: &ChangesetResult) -> usize {
    result
        .files
        .iter()
        .filter(|file| file.status.is_published())
        .count()
}

fn first_conflict(result: &ChangesetResult) -> Option<&FileResult> {
    result.files.iter().find(|file| !file.status.is_success())
}

const fn occ_status_wire(status: OccStatus) -> &'static str {
    match status {
        OccStatus::Accepted => "accepted",
        OccStatus::Committed => "committed",
        OccStatus::AbortedVersion => "aborted_version",
        OccStatus::AbortedOverlap => "aborted_overlap",
        OccStatus::Dropped => "dropped",
        OccStatus::Rejected => "rejected",
        _ => "failed",
    }
}

pub(crate) fn guarded_conflict_response(
    verb: &str,
    path: &str,
    status: &str,
    reason: &str,
    message: &str,
    mut timings: serde_json::Map<String, Value>,
    total_start: Instant,
) -> Value {
    timings.insert(
        format!("api.{verb}.total_s"),
        json!(total_start.elapsed().as_secs_f64()),
    );
    let mut response = json!({
        "success": false,
        "workspace": "ephemeral",
        "changed_paths": [],
        "changed_path_kinds": {},
        "mutation_source": mutation_source(verb),
        "status": status,
        "conflict": {
            "reason": reason,
            "conflict_file": path,
            "message": message,
        },
        "conflict_reason": reason,
        "error": null,
        "timings": Value::Object(timings),
    });
    if verb == "edit" {
        response["applied_edits"] = json!(0);
    }
    response
}

pub(crate) fn resource_timings(
    manifest: &Manifest,
    changed_path_count: usize,
) -> serde_json::Map<String, Value> {
    let mut timings = serde_json::Map::new();
    timings.insert(
        "resource.command_exec.changed_path_count".to_owned(),
        json!(usize_to_f64_saturating(changed_path_count)),
    );
    timings.insert(
        "resource.layer_stack.manifest_depth".to_owned(),
        json!(usize_to_f64_saturating(manifest.depth())),
    );
    timings.insert(
        "resource.layer_stack.manifest_path_count".to_owned(),
        json!(usize_to_f64_saturating(manifest.depth())),
    );
    for key in [
        "resource.command_exec.run_dir_tree_exists",
        "resource.command_exec.run_dir_tree_bytes",
        "resource.command_exec.run_dir_tree_file_count",
        "resource.command_exec.run_dir_tree_dir_count",
        "resource.command_exec.run_dir_tree_entry_count",
        "resource.command_exec.run_dir_tree_truncated",
        "resource.command_exec.workspace_tree_exists",
        "resource.command_exec.workspace_tree_bytes",
        "resource.command_exec.workspace_tree_file_count",
        "resource.command_exec.workspace_tree_dir_count",
        "resource.command_exec.workspace_tree_entry_count",
        "resource.command_exec.workspace_tree_truncated",
        "resource.command_exec.upperdir_tree_exists",
        "resource.command_exec.upperdir_tree_bytes",
        "resource.command_exec.upperdir_tree_file_count",
        "resource.command_exec.upperdir_tree_dir_count",
        "resource.command_exec.upperdir_tree_entry_count",
        "resource.command_exec.upperdir_tree_truncated",
    ] {
        timings.insert(key.to_owned(), json!(0.0));
    }
    insert_cgroup_resource_timings(&mut timings);
    timings
}

pub(crate) fn insert_tree_resource_timings(
    timings: &mut serde_json::Map<String, Value>,
    prefix: &str,
    stats: &TreeResourceStats,
) {
    timings.insert(format!("{prefix}_tree_exists"), json!(stats.exists));
    timings.insert(format!("{prefix}_tree_bytes"), json!(stats.bytes));
    timings.insert(format!("{prefix}_tree_file_count"), json!(stats.file_count));
    timings.insert(format!("{prefix}_tree_dir_count"), json!(stats.dir_count));
    timings.insert(
        format!("{prefix}_tree_entry_count"),
        json!(stats.entry_count),
    );
    timings.insert(format!("{prefix}_tree_truncated"), json!(stats.truncated));
}

fn allocated_bytes(metadata: &fs::Metadata) -> f64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let allocated = metadata.blocks().saturating_mul(512);
        u64_to_f64_saturating(if allocated > 0 {
            allocated
        } else {
            metadata.len()
        })
    }
    #[cfg(not(unix))]
    {
        u64_to_f64_saturating(metadata.len())
    }
}

fn insert_cgroup_resource_timings(timings: &mut serde_json::Map<String, Value>) {
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

    let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/io.stat") else {
        return;
    };
    let mut totals = BTreeMap::<&str, f64>::from([
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

fn mutation_source(verb: &str) -> &'static str {
    match verb {
        "write" => "api_write",
        "edit" => "api_edit",
        "exec_command" => "overlay_capture",
        "plugin_overlay" => "plugin_overlay",
        _ => "",
    }
}

pub(crate) fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn usize_to_f64_saturating(value: usize) -> f64 {
    u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

pub(crate) fn i64_to_f64_saturating(value: i64) -> f64 {
    u64::try_from(value).map_or(0.0, u64_to_f64_saturating)
}

pub(crate) fn u64_to_f64_saturating(value: u64) -> f64 {
    u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

pub(crate) fn u64_to_usize_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

pub(crate) fn f64_to_i64_rounded_saturating(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            i64::MIN
        } else {
            i64::MAX
        };
    }
    let rounded = value.round();
    format!("{rounded:.0}").parse::<i64>().unwrap_or_else(|_| {
        if rounded.is_sign_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

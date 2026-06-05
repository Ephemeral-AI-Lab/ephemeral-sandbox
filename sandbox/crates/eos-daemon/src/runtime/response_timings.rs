//! Shared daemon response shaping and resource timing helpers.

use std::collections::BTreeMap;
use std::time::Instant;

use eos_occ::ChangesetResult;
use eos_protocol::Manifest;
use eos_runner::RunResult;
use eos_workspace_api::WorkspaceTimings;
use serde_json::{json, Value};

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
    pub(crate) fn from_ephemeral(stats: &eos_ephemeral_workspace::TreeResourceStats) -> Self {
        let file_entries = stats.files.saturating_add(stats.symlinks);
        let entry_count = file_entries.saturating_add(stats.dirs);
        Self {
            exists: if entry_count > 0 { 1.0 } else { 0.0 },
            bytes: u64_to_f64_saturating(stats.bytes),
            file_count: u64_to_f64_saturating(file_entries),
            dir_count: u64_to_f64_saturating(stats.dirs),
            entry_count: u64_to_f64_saturating(entry_count),
            truncated: 0.0,
        }
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

pub(crate) fn timing_map(timings: serde_json::Map<String, Value>) -> WorkspaceTimings {
    timings.into_iter().collect()
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
    let changed_paths = result.published_paths();
    let mut changed_path_kinds = serde_json::Map::new();
    for path in &changed_paths {
        changed_path_kinds.insert(path.to_owned(), json!("write"));
    }
    let conflict = result.first_conflict();
    let mut response = json!({
        "success": result.success(),
        "workspace": "ephemeral",
        "changed_paths": changed_paths,
        "changed_path_kinds": Value::Object(changed_path_kinds),
        "mutation_source": mutation_source(verb),
        "status": conflict
            .as_ref()
            .map_or("committed", |file| file.status.wire_str()),
        "conflict": conflict.as_ref().map(|file| json!({
            "reason": file.status.wire_str(),
            "conflict_file": file.path.as_str(),
            "message": file.conflict_message(file.status.wire_str()),
        })),
        "conflict_reason": conflict.as_ref().map(|file| {
            file.conflict_message(file.status.wire_str())
        }),
        "error": null,
        "timings": Value::Object(timings),
    });
    if let Some(count) = applied_edits {
        response["applied_edits"] = json!(count);
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
    value.round() as i64
}

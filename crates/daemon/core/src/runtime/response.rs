//! Shared daemon response shaping and resource timing helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Instant;

use layerstack::ChangesetResult;
use layerstack::Manifest;
use namespace::protocol::RunResult;
use operation::{
    ChangedPathKind, ChangedPathKinds, MutationCore, MutationSource, MutationStatus,
    WorkspaceConflict, WorkspaceKind,
};
use serde::Serialize;
use trace::usize_to_f64_saturating;

pub(crate) fn u64_to_f64_saturating(value: u64) -> f64 {
    const U32_FACTOR: f64 = 4_294_967_296.0;
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high).mul_add(U32_FACTOR, f64::from(low))
}
use serde_json::{json, Value};

#[derive(Clone, Debug, Default)]
pub(crate) struct TreeResourceStats {
    exists: f64,
    bytes: f64,
    file_count: f64,
    dir_count: f64,
    entry_count: f64,
    truncated: f64,
    read_error_count: f64,
    first_error_path: Option<String>,
}

impl TreeResourceStats {
    pub(crate) fn from_ephemeral(stats: &workspace::TreeResourceStats) -> Self {
        let file_entries = stats.files.saturating_add(stats.symlinks);
        let entry_count = file_entries.saturating_add(stats.dirs);
        Self {
            exists: if entry_count > 0 { 1.0 } else { 0.0 },
            bytes: u64_to_f64_saturating(stats.bytes),
            file_count: u64_to_f64_saturating(file_entries),
            dir_count: u64_to_f64_saturating(stats.dirs),
            entry_count: u64_to_f64_saturating(entry_count),
            truncated: if stats.truncated { 1.0 } else { 0.0 },
            read_error_count: u64_to_f64_saturating(stats.read_error_count),
            first_error_path: stats.first_error_path.clone(),
        }
    }
}

pub(crate) fn attach_runner_shell_fields(response: &mut Value, runner: &RunResult) {
    response["exit_code"] = runner
        .payload
        .get("exit_code")
        .cloned()
        .unwrap_or_else(|| json!(runner.exit_code));
    response["stdout"] = runner
        .payload
        .get("stdout")
        .cloned()
        .unwrap_or_else(|| json!(""));
    response["stderr"] = runner
        .payload
        .get("stderr")
        .cloned()
        .unwrap_or_else(|| json!(""));
    response["warnings"] = runner
        .payload
        .get("warnings")
        .cloned()
        .unwrap_or_else(|| json!([]));
}

pub(crate) fn copy_runner_timings(
    timings: &mut serde_json::Map<String, Value>,
    runner: &RunResult,
) {
    if let Some(runner_timings) = runner.payload.get("timings").and_then(Value::as_object) {
        for (key, value) in runner_timings {
            timings.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
}

#[derive(Serialize)]
struct PluginOverlayMutationResponse {
    #[serde(flatten)]
    core: MutationCore,
    workspace: WorkspaceKind,
    status: MutationStatus,
}

/// Daemon-owned plugin-overlay response synthesis over [`MutationCore`].
/// The mutation source is the typed [`MutationSource::PluginOverlay`] variant,
/// set before serialization — no post-hoc string splice and no `error: null`
/// placeholder. Failure detail is materialized by `apply_plugin_overlay_status`.
pub(crate) fn plugin_overlay_changeset_response(
    result: &ChangesetResult,
    mut timings: serde_json::Map<String, Value>,
    total_start: Instant,
) -> Value {
    for (key, value) in &result.timings {
        timings.insert(key.clone(), json!(value));
    }
    timings.insert(
        "sandbox.plugin.overlay.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    let changed_paths = result.published_paths();
    let mut changed_path_kinds = ChangedPathKinds::new();
    for path in &changed_paths {
        changed_path_kinds.insert(path.to_owned(), ChangedPathKind::Write);
    }
    let conflict = result.first_conflict();
    serde_json::to_value(PluginOverlayMutationResponse {
        core: MutationCore {
            success: result.success(),
            changed_paths,
            changed_path_kinds,
            mutation_source: Some(MutationSource::PluginOverlay),
            conflict: conflict.as_ref().map(|file| {
                let reason = file.status.wire_str();
                WorkspaceConflict::path(reason, file.path.as_str(), file.conflict_message(reason))
            }),
            conflict_reason: conflict
                .as_ref()
                .map(|file| file.conflict_message(file.status.wire_str()).to_owned()),
            timings: timings.into_iter().collect(),
        },
        workspace: WorkspaceKind::Ephemeral,
        status: conflict
            .as_ref()
            .map_or(MutationStatus::Committed, |file| file.status.into()),
    })
    .expect("changeset mutation response serializes")
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
        json!(usize_to_f64_saturating(manifest.layers.len())),
    );
    // Tree stats appear only when a path actually paid for a walk; absence
    // means "not sampled", never a fabricated zero walk.
    insert_cgroup_process_resource_timings(&mut timings);
    timings
}

pub(crate) fn insert_cgroup_process_resource_timings(timings: &mut serde_json::Map<String, Value>) {
    let sampler_start = Instant::now();
    insert_cgroup_resource_timings(timings);
    insert_process_resource_timings(timings);
    timings.insert(
        "resource.sampler.cgroup_process_duration_us".to_owned(),
        json!(sampler_start.elapsed().as_micros()),
    );
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
    timings.insert(
        format!("{prefix}_tree_read_error_count"),
        json!(stats.read_error_count),
    );
    if let Some(path) = &stats.first_error_path {
        timings.insert(format!("{prefix}_tree_first_error_path"), json!(path));
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

    for (path, key) in [
        (
            "/sys/fs/cgroup/memory.current",
            "resource.cgroup.memory_current_bytes",
        ),
        (
            "/sys/fs/cgroup/memory.peak",
            "resource.cgroup.memory_peak_bytes",
        ),
        (
            "/sys/fs/cgroup/memory.swap.current",
            "resource.cgroup.memory_swap_current_bytes",
        ),
        (
            "/sys/fs/cgroup/memory.swap.peak",
            "resource.cgroup.memory_swap_peak_bytes",
        ),
    ] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(value) = raw.trim().parse::<f64>() {
                timings.insert(key.to_owned(), json!(value));
            }
        }
    }

    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory.events") {
        for line in raw.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else {
                continue;
            };
            let Some(value) = parts.next().and_then(|raw| raw.parse::<f64>().ok()) else {
                continue;
            };
            timings.insert(
                format!("resource.cgroup.memory_events_{name}"),
                json!(value),
            );
        }
    }

    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/io.stat") {
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

    for (path, prefix) in [
        ("/sys/fs/cgroup/cpu.pressure", "cpu"),
        ("/sys/fs/cgroup/memory.pressure", "memory"),
        ("/sys/fs/cgroup/io.pressure", "io"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            insert_pressure_timings(timings, prefix, &raw);
        }
    }
}

fn insert_pressure_timings(timings: &mut serde_json::Map<String, Value>, prefix: &str, raw: &str) {
    for (key, value) in parse_pressure_metrics(prefix, raw) {
        timings.insert(format!("resource.cgroup.psi_{key}"), json!(value));
    }
}

fn parse_pressure_metrics(prefix: &str, raw: &str) -> BTreeMap<String, f64> {
    let mut metrics = BTreeMap::new();
    for line in raw.lines() {
        let mut tokens = line.split_whitespace();
        let Some(level @ ("some" | "full")) = tokens.next() else {
            continue;
        };
        for token in tokens {
            let Some((name @ ("avg10" | "avg60" | "avg300" | "total"), raw_value)) =
                token.split_once('=')
            else {
                continue;
            };
            if let Ok(value) = raw_value.parse::<f64>() {
                metrics.insert(format!("{prefix}_{level}_{name}"), value);
            }
        }
    }
    metrics
}

/// Emit daemon process memory from `/proc/self/status`: `VmRSS` (current
/// resident set) and `VmHWM` (peak resident set), reported in bytes. These are
/// gauges, not run deltas, and are absent on non-Linux dev hosts where the file
/// does not exist.
fn insert_process_resource_timings(timings: &mut serde_json::Map<String, Value>) {
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

#[cfg(test)]
#[path = "../../tests/unit/response/mod.rs"]
mod tests;

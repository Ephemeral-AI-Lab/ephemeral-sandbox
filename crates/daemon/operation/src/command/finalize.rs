use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use layerstack::service::Snapshot;
use layerstack::{service, FileResult};
use layerstack::{CaptureRouteStats, ChangesetResult, CommitOptions, CommitStatus};
use serde_json::{json, Map, Value};
use trace::usize_to_f64_saturating;
use workspace::IsolatedWorkspaceBinding;
use workspace::{
    capture_upperdir, capture_upperdir_for_snapshot, EphemeralWorkspace, RoutedCapturedChanges,
    TreeResourceStats,
};

use super::contract::{
    u64_to_f64_saturating, CommandMetadata, CommandResponse, IgnoredPublishLaneMetadata,
    PublishLanesMetadata, SourcePublishLaneMetadata,
};
use super::outcome::{
    ChangedPathKinds, FinalizeCommandRequest, MutationSource, WorkspaceApiError, WorkspaceConflict,
    WorkspaceKind, WorkspaceTimings,
};
use crate::core::changed_path_kind_pairs;
use crate::{CommandId, MutationCore};

pub(crate) fn finalize_ephemeral_command(
    root: &Path,
    snapshot: &Snapshot,
    workspace: &EphemeralWorkspace,
    commit_options: CommitOptions,
    request: FinalizeCommandRequest,
) -> Result<CommandResponse, WorkspaceApiError> {
    let mut timings = base_timings(root)?;
    let command_success = request.command_succeeded();
    let spool_dir = workspace
        .dirs()
        .run_dir
        .join("spool")
        .join("publish-capture");
    let captured = match capture_upperdir_for_snapshot(
        root,
        snapshot,
        &workspace.dirs().upperdir,
        &spool_dir,
        command_success,
    ) {
        Ok(captured) => captured,
        Err(error) if !command_success => {
            timings.insert(
                "command_exec.capture_upperdir_error".to_owned(),
                json!(error.to_string()),
            );
            copy_runner_timings(&mut timings, request.runner_result.as_ref());
            insert_command_timings(&mut timings, 0, 0.0, 0.0, request.command_elapsed_s, false);
            return Ok(command_response(
                WorkspaceKind::Ephemeral,
                request,
                CommandFinalization {
                    success: false,
                    changed_paths: Vec::new(),
                    changed_path_kinds: ChangedPathKinds::default(),
                    mutation_source: None,
                    conflict: None,
                    conflict_reason: None,
                    timings,
                    extras: publish_lanes_extras(PublishLanesMetadata::dropped_command_failed(
                        snapshot.manifest_version,
                    )),
                },
            ));
        }
        Err(error) => return Err(finalize_error(error)),
    };
    let RoutedCapturedChanges {
        captured: captured_changes,
        route_stats,
        metadata_path_count: captured_path_count,
        spool_dir,
    } = captured;
    let _spool_cleanup = SpoolCleanup::new(spool_dir);
    let changed_path_kinds: ChangedPathKinds =
        changed_path_kind_pairs(&captured_changes.changes).collect();

    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &captured_changes.stats,
    );
    timings.insert(
        "resource.command_exec.upperdir_tree_sampler_duration_us".to_owned(),
        json!(captured_changes.capture_s * 1_000_000.0),
    );
    let run_dir_walk_start = Instant::now();
    let run_dir_stats = TreeResourceStats::collect(&workspace.dirs().run_dir);
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.run_dir",
        &run_dir_stats,
    );
    timings.insert(
        "resource.command_exec.run_dir_tree_sampler_duration_us".to_owned(),
        json!(u64::try_from(run_dir_walk_start.elapsed().as_micros()).unwrap_or(u64::MAX)),
    );
    copy_runner_timings(&mut timings, request.runner_result.as_ref());

    if !command_success {
        let publish_lanes = publish_lanes_with_route_drop_summary(
            PublishLanesMetadata::dropped_command_failed_with_counts(
                route_stats.gated_path_count,
                route_stats.direct_path_count,
                route_stats.direct_bytes,
                snapshot.manifest_version,
            ),
            route_stats.drop_path_count,
            route_stats.drop_reason_counts.clone(),
        );
        insert_command_timings(
            &mut timings,
            captured_path_count,
            captured_changes.capture_s,
            0.0,
            request.command_elapsed_s,
            false,
        );
        return Ok(command_response(
            WorkspaceKind::Ephemeral,
            request,
            CommandFinalization {
                success: false,
                changed_paths: Vec::new(),
                changed_path_kinds: ChangedPathKinds::default(),
                mutation_source: None,
                conflict: None,
                conflict_reason: None,
                timings,
                extras: publish_lanes_extras(publish_lanes),
            },
        ));
    }

    let publish_start = std::time::Instant::now();
    let changeset = service::publish_command_capture_lane_aware(
        root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured_changes.changes,
        &captured_changes.protected_drops,
        commit_options,
    )
    .map_err(finalize_error)?;
    let publish_s = publish_start.elapsed().as_secs_f64();

    let first_conflict = changeset.first_conflict();
    let publish_success = changeset.success();
    let publish_lanes =
        publish_lanes_from_changeset(&changeset, route_stats, snapshot.manifest_version);

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
        captured_path_count,
        captured_changes.capture_s,
        occ_s,
        request.command_elapsed_s,
        false,
    );

    Ok(command_response(
        WorkspaceKind::Ephemeral,
        request,
        CommandFinalization {
            success: command_success && publish_success,
            changed_paths: changeset.published_paths(),
            changed_path_kinds,
            mutation_source: Some(MutationSource::OverlayCapture),
            conflict: first_conflict.map(conflict_from_file),
            conflict_reason: first_conflict.map(|file| conflict_message(file).to_owned()),
            timings,
            extras: publish_lanes_extras(publish_lanes),
        },
    ))
}

pub(crate) fn finalize_isolated_command(
    binding: &IsolatedWorkspaceBinding,
    request: FinalizeCommandRequest,
) -> Result<CommandResponse, WorkspaceApiError> {
    let mut timings = base_timings(&binding.layer_stack_root)?;
    let captured = capture_upperdir(&binding.upperdir)
        .map_err(|err| finalize_error(format!("capture isolated upperdir: {err}")))?;
    let changed_path_kinds: ChangedPathKinds = changed_path_kind_pairs(&captured.changes).collect();
    let changed_paths: Vec<String> = changed_path_kinds.keys().cloned().collect();
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &captured.stats,
    );
    timings.insert(
        "resource.command_exec.upperdir_tree_sampler_duration_us".to_owned(),
        json!(captured.capture_s * 1_000_000.0),
    );
    copy_runner_timings(&mut timings, request.runner_result.as_ref());
    let command_success = request.command_succeeded();
    insert_command_timings(
        &mut timings,
        changed_paths.len(),
        captured.capture_s,
        0.0,
        request.command_elapsed_s,
        true,
    );
    let mut extras = Map::new();
    extras.insert(
        "isolated_workspace".to_owned(),
        json!({
            "caller_id": binding.caller_id,
            "workspace_handle_id": binding.workspace_handle_id,
            "manifest_version": binding.manifest_version,
            "manifest_root_hash": binding.manifest_root_hash,
            "published": false,
        }),
    );
    extras.insert("warnings".to_owned(), json!([]));
    PublishLanesMetadata::empty(binding.manifest_version).insert_into(&mut extras);
    let mut response = command_response(
        WorkspaceKind::Isolated,
        request,
        CommandFinalization {
            success: command_success,
            changed_paths,
            changed_path_kinds,
            mutation_source: Some(MutationSource::IsolatedWorkspace),
            conflict: None,
            conflict_reason: None,
            timings,
            extras,
        },
    );
    response.exit_code = Some(response.exit_code.unwrap_or(1));
    Ok(response)
}

pub(crate) fn discarded_response(
    workspace_kind: WorkspaceKind,
    request: FinalizeCommandRequest,
    route_manifest_version: Option<i64>,
) -> CommandResponse {
    let extras = route_manifest_version.map_or_else(Map::new, |route_manifest_version| {
        publish_lanes_extras(PublishLanesMetadata::dropped_command_failed(
            route_manifest_version,
        ))
    });
    command_response(
        workspace_kind,
        request,
        CommandFinalization {
            success: false,
            changed_paths: Vec::new(),
            changed_path_kinds: ChangedPathKinds::default(),
            mutation_source: None,
            conflict: None,
            conflict_reason: None,
            timings: WorkspaceTimings::default(),
            extras,
        },
    )
}

struct CommandFinalization {
    success: bool,
    changed_paths: Vec<String>,
    changed_path_kinds: ChangedPathKinds,
    mutation_source: Option<MutationSource>,
    conflict: Option<WorkspaceConflict>,
    conflict_reason: Option<String>,
    timings: WorkspaceTimings,
    extras: Map<String, Value>,
}

struct SpoolCleanup {
    path: Option<std::path::PathBuf>,
}

impl SpoolCleanup {
    fn new(path: Option<std::path::PathBuf>) -> Self {
        Self { path }
    }
}

impl Drop for SpoolCleanup {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn command_response(
    workspace_kind: WorkspaceKind,
    request: FinalizeCommandRequest,
    finalization: CommandFinalization,
) -> CommandResponse {
    CommandResponse {
        status: request.status,
        exit_code: request.exit_code,
        stdout: request.stdout,
        stderr: request.stderr,
        command_id: request.command_id.map(CommandId::new),
        finalized: Some(CommandMetadata {
            core: MutationCore {
                success: finalization.success,
                changed_paths: finalization.changed_paths,
                changed_path_kinds: finalization.changed_path_kinds,
                mutation_source: finalization.mutation_source,
                conflict: finalization.conflict,
                conflict_reason: finalization.conflict_reason,
                timings: finalization.timings,
            },
            workspace: workspace_kind,
            extras: finalization.extras,
        }),
    }
}

fn publish_lanes_extras(publish_lanes: PublishLanesMetadata) -> Map<String, Value> {
    let mut extras = Map::new();
    publish_lanes.insert_into(&mut extras);
    extras
}

fn publish_lanes_from_changeset(
    changeset: &ChangesetResult,
    route_stats: CaptureRouteStats,
    route_manifest_version: i64,
) -> PublishLanesMetadata {
    let source_path_count = route_stats.gated_path_count;
    let source_status = source_publish_status(changeset, source_path_count);
    let (ignored_status, ignored_mode, ignored_drop_reason) =
        ignored_publish_outcome(changeset, &route_stats, source_status);

    let publish_lanes = PublishLanesMetadata::new(
        SourcePublishLaneMetadata::new(source_path_count, source_status, None::<String>),
        IgnoredPublishLaneMetadata::new(
            route_stats.direct_path_count,
            route_stats.direct_bytes,
            route_stats.direct_spooled_bytes,
            ignored_status,
            ignored_mode,
            ignored_drop_reason,
        ),
        route_manifest_version,
    );
    publish_lanes_with_route_drop_summary(
        publish_lanes,
        route_stats.drop_path_count,
        route_stats.drop_reason_counts,
    )
}

fn publish_lanes_with_route_drop_summary(
    mut publish_lanes: PublishLanesMetadata,
    dropped_path_count: usize,
    drop_reason_counts: BTreeMap<String, usize>,
) -> PublishLanesMetadata {
    publish_lanes.routing.dropped_path_count = dropped_path_count;
    publish_lanes.routing.drop_reason_counts = drop_reason_counts;
    publish_lanes
}

fn source_publish_status(changeset: &ChangesetResult, source_path_count: usize) -> &'static str {
    if source_path_count == 0 {
        return "empty";
    }
    if changeset
        .files
        .iter()
        .any(|file| file.status == CommitStatus::Failed)
    {
        return "failed";
    }
    if changeset
        .files
        .iter()
        .any(|file| file.status == CommitStatus::AbortedVersion)
    {
        return "conflict";
    }
    if changeset.published_manifest_version.is_some() {
        "committed"
    } else {
        "accepted_noop"
    }
}

fn ignored_publish_outcome(
    changeset: &ChangesetResult,
    route_stats: &CaptureRouteStats,
    source_status: &str,
) -> (&'static str, Option<&'static str>, Option<String>) {
    if route_stats.direct_path_count == 0 {
        return ("empty", None, None);
    }
    if matches!(source_status, "conflict" | "failed") {
        return (
            "dropped_due_to_source_conflict",
            None,
            Some("source_not_published".to_owned()),
        );
    }
    if let Some(reason) = route_stats.ignored_limit_drop_reason.as_deref() {
        return ("dropped_due_to_limits", None, Some(reason.to_owned()));
    }
    if changeset.success() {
        return ("published_lww", Some("direct_lww"), None);
    }
    ("failed", None, Some("publish_failed".to_owned()))
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
        ("sandbox.command.exec.dispatch_total_s", elapsed_s),
    ] {
        timings.insert(key.to_owned(), json!(value));
    }
    if include_api_total {
        timings.insert("sandbox.command.exec.total_s".to_owned(), json!(elapsed_s));
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
    // Tree stats are inserted only by the paths that actually walk a tree;
    // an absent key means "not sampled", never a fabricated zero walk.
    insert_cgroup_process_resource_timings(&mut timings);
    Ok(timings)
}

pub(crate) fn insert_cgroup_process_resource_timings(timings: &mut WorkspaceTimings) {
    let sampler_start = Instant::now();
    insert_cgroup_resource_timings(timings);
    insert_process_resource_timings(timings);
    timings.insert(
        "resource.sampler.cgroup_process_duration_us".to_owned(),
        json!(sampler_start.elapsed().as_micros()),
    );
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

fn insert_pressure_timings(timings: &mut WorkspaceTimings, prefix: &str, raw: &str) {
    for (key, value) in parse_pressure_metrics(prefix, raw) {
        timings.insert(format!("resource.cgroup.psi_{key}"), json!(value));
    }
}

fn parse_pressure_metrics(prefix: &str, raw: &str) -> std::collections::BTreeMap<String, f64> {
    let mut metrics = std::collections::BTreeMap::new();
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

fn copy_runner_timings(timings: &mut WorkspaceTimings, runner_result: Option<&Value>) {
    let Some(runner_timings) = runner_result
        .and_then(|result| {
            result
                .get("payload")
                .and_then(|payload| payload.get("timings"))
                .or_else(|| result.get("timings"))
        })
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
    insert_resource_timing(
        timings,
        &format!("{prefix}_tree_truncated"),
        u64::from(stats.truncated),
    );
    insert_resource_timing(
        timings,
        &format!("{prefix}_tree_read_error_count"),
        stats.read_error_count,
    );
    if let Some(path) = &stats.first_error_path {
        timings.insert(format!("{prefix}_tree_first_error_path"), json!(path));
    }
}

fn insert_resource_timing(timings: &mut WorkspaceTimings, key: &str, value: u64) {
    timings.insert(key.to_owned(), json!(u64_to_f64_saturating(value)));
}

fn finalize_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("command_finalize_failed", error.to_string())
}

#[cfg(test)]
mod tests {
    use crate::command::CommandStatus;
    use layerstack::{LayerChange, LayerStack};
    use serde_json::Value;
    use workspace::TreeResourceStats;

    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn pressure_metrics_parse_some_and_full_levels() {
        let metrics = parse_pressure_metrics(
            "io",
            "some avg10=2.50 avg60=1.50 avg300=0.50 total=100\nfull avg10=0.75 avg60=0.25 avg300=0.05 total=9\n",
        );

        assert_eq!(metrics.get("io_some_avg10").copied(), Some(2.5));
        assert_eq!(metrics.get("io_some_total").copied(), Some(100.0));
        assert_eq!(metrics.get("io_full_avg300").copied(), Some(0.05));
        assert_eq!(metrics.get("io_full_total").copied(), Some(9.0));
    }

    #[test]
    fn tree_resource_timings_forward_truncation_marker() {
        let mut timings = crate::WorkspaceTimings::new();
        let stats = TreeResourceStats {
            files: 1,
            dirs: 1,
            symlinks: 0,
            bytes: 10,
            truncated: true,
            read_error_count: 1,
            first_error_path: Some("/tmp/missing".to_owned()),
        };

        insert_tree_resource_timings(&mut timings, "resource.command_exec.upperdir", &stats);

        assert_eq!(
            timings["resource.command_exec.upperdir_tree_truncated"],
            serde_json::json!(1.0)
        );
        assert_eq!(
            timings["resource.command_exec.upperdir_tree_read_error_count"],
            serde_json::json!(1.0)
        );
        assert_eq!(
            timings["resource.command_exec.upperdir_tree_first_error_path"],
            serde_json::json!("/tmp/missing")
        );
    }

    #[test]
    fn copy_runner_timings_reads_runner_payload_shape() {
        let mut timings = crate::WorkspaceTimings::new();
        let runner_result = serde_json::json!({
            "exit_code": 0,
            "payload": {
                "timings": {
                    "workspace.mount_s": 0.012,
                    "workspace.shell_spawn_s": 0.034
                }
            }
        });

        copy_runner_timings(&mut timings, Some(&runner_result));

        assert_eq!(timings["workspace.mount_s"], serde_json::json!(0.012));
        assert_eq!(timings["workspace.shell_spawn_s"], serde_json::json!(0.034));
    }

    #[test]
    fn nonzero_ephemeral_command_discards_upperdir_and_reports_lanes() -> TestResult {
        assert_non_success_discards(CommandStatus::Error, Some(42))
    }

    #[test]
    fn nonzero_ephemeral_command_reports_lanes_without_reading_large_ignored_payload() -> TestResult
    {
        let fixture = EphemeralFinalizeFixture::new("large-ignored-error")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace = EphemeralWorkspace::create(&fixture.scratch, "command", "large-ignored")?;
        write_upperdir_sparse_file(&workspace, "ignored/huge.bin", (8 * 1024 * 1024) + 1)?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Error,
                exit_code: Some(42),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_oversized_non_success".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "error");
        assert_eq!(wire["exit_code"], 42);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(
            wire["publish_lanes"]["source"]["publish_status"],
            "dropped_command_failed"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_status"],
            "dropped_command_failed"
        );
        assert_eq!(wire["publish_lanes"]["source"]["path_count"], 0);
        assert_eq!(wire["publish_lanes"]["ignored"]["path_count"], 1);
        assert_eq!(
            wire["publish_lanes"]["ignored"]["bytes"],
            serde_json::json!((8 * 1024 * 1024) + 1)
        );
        assert_eq!(wire["publish_lanes"]["ignored"]["spooled_bytes"], 0);
        let timings = &response
            .finalized
            .as_ref()
            .expect("finalized response")
            .core
            .timings;
        assert!(
            timings
                .get("command_exec.capture_upperdir_error")
                .and_then(Value::as_str)
                .is_none(),
            "non-success metadata-first capture should not read large ignored payloads: {timings:?}"
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            "non-success command must not advance the manifest"
        );
        let (_bytes, exists) =
            LayerStack::open(fixture.root.clone())?.read_bytes("ignored/huge.bin")?;
        assert!(!exists, "oversized failed-command payload must not publish");
        Ok(())
    }

    #[test]
    fn timed_out_ephemeral_command_discards_upperdir_and_reports_lanes() -> TestResult {
        assert_non_success_discards(CommandStatus::TimedOut, Some(124))
    }

    #[test]
    fn cancelled_ephemeral_command_discards_upperdir_and_reports_lanes() -> TestResult {
        assert_non_success_discards(CommandStatus::Cancelled, Some(130))
    }

    #[test]
    fn successful_ephemeral_command_reports_git_metadata_drop_without_publishing() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("git-metadata-drop")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "git-metadata-drop")?;
        write_upperdir_file(
            &workspace,
            ".git/config",
            b"[core]\nrepositoryformatversion = 0\n",
        )?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_git_metadata_drop".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(wire["publish_lanes"]["source"]["publish_status"], "empty");
        assert_eq!(wire["publish_lanes"]["ignored"]["publish_status"], "empty");
        assert_eq!(wire["publish_lanes"]["ignored"]["path_count"], 0);
        assert_eq!(
            wire["publish_lanes"]["routing"]["dropped_path_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"git_metadata_unsupported": 1})
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["route_manifest_version"],
            snapshot.manifest_version
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            ".git-only dropped command must not advance the manifest"
        );
        let (_bytes, exists) = LayerStack::open(fixture.root.clone())?.read_bytes(".git/config")?;
        assert!(
            !exists,
            ".git metadata must not publish through command finalize"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn successful_ephemeral_command_reports_unsupported_special_file_drop() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("unsupported-special-drop")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "unsupported-special-drop")?;
        let fifo_path = workspace.dirs().upperdir.join("run.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()?;
        assert!(status.success(), "mkfifo failed with status {status}");

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_unsupported_special_drop".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(wire["publish_lanes"]["source"]["publish_status"], "empty");
        assert_eq!(wire["publish_lanes"]["ignored"]["publish_status"], "empty");
        assert_eq!(
            wire["publish_lanes"]["routing"]["dropped_path_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"unsupported_special_file": 1})
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            "special-file-only dropped command must not advance the manifest"
        );
        let (_bytes, exists) = LayerStack::open(fixture.root.clone())?.read_bytes("run.fifo")?;
        assert!(
            !exists,
            "unsupported special file must not publish through command finalize"
        );
        Ok(())
    }

    #[test]
    fn successful_ephemeral_command_reports_daemon_control_path_drop() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("daemon-control-drop")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "daemon-control-drop")?;
        write_upperdir_file(&workspace, "manifest.json", b"{\"version\":999}")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_daemon_control_drop".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(wire["publish_lanes"]["source"]["publish_status"], "empty");
        assert_eq!(wire["publish_lanes"]["ignored"]["publish_status"], "empty");
        assert_eq!(
            wire["publish_lanes"]["routing"]["dropped_path_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"daemon_control_path": 1})
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            "daemon-control-only dropped command must not advance the manifest"
        );
        let (_bytes, exists) =
            LayerStack::open(fixture.root.clone())?.read_bytes("manifest.json")?;
        assert!(
            !exists,
            "daemon control path must not publish through command finalize"
        );
        Ok(())
    }

    #[test]
    fn successful_ephemeral_command_reports_command_scratch_path_drop() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("command-scratch-drop")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "command-scratch-drop")?;
        write_upperdir_file(&workspace, "transcript.log", b"private transcript")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_command_scratch_drop".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(wire["publish_lanes"]["source"]["publish_status"], "empty");
        assert_eq!(wire["publish_lanes"]["ignored"]["publish_status"], "empty");
        assert_eq!(
            wire["publish_lanes"]["routing"]["dropped_path_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"command_scratch_path": 1})
        );
        let (_bytes, exists) =
            LayerStack::open(fixture.root.clone())?.read_bytes("transcript.log")?;
        assert!(
            !exists,
            "command scratch path must not publish through command finalize"
        );
        Ok(())
    }

    #[test]
    fn successful_ephemeral_command_reports_invalid_layer_path_drop() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("invalid-layer-path-drop")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "invalid-layer-path-drop")?;
        write_upperdir_file(&workspace, ".wh...", b"")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_invalid_layer_path_drop".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(
            wire["publish_lanes"]["routing"]["dropped_path_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"invalid_layer_path": 1})
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            "invalid-layer-path-only dropped command must not advance the manifest"
        );
        Ok(())
    }

    #[test]
    fn successful_ephemeral_command_reports_opaque_mixed_route_drop() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("opaque-mixed-route-drop")?;
        LayerStack::open(fixture.root.clone())?.publish_layer(&[
            LayerChange::Write {
                path: layerstack::LayerPath::parse("tree/src.txt")?,
                content: b"source".to_vec(),
            },
            LayerChange::Write {
                path: layerstack::LayerPath::parse("tree/ignored/cache.txt")?,
                content: b"ignored".to_vec(),
            },
        ])?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "opaque-mixed-route-drop")?;
        write_upperdir_opaque_marker(&workspace, "tree")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_opaque_mixed_route_drop".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], false);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(
            wire["publish_lanes"]["routing"]["dropped_path_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"opaque_dir_mixed_routes": 1})
        );
        assert_eq!(
            wire["publish_lanes"]["routing"]["route_manifest_version"],
            snapshot.manifest_version
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            "mixed-route opaque marker must not advance the manifest"
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("tree/src.txt")?
                .0,
            "source"
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("tree/ignored/cache.txt")?
                .0,
            "ignored"
        );
        Ok(())
    }

    #[test]
    fn successful_ephemeral_command_drops_oversized_ignored_but_publishes_source() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("ignored-limit-source-publish")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "ignored-limit-source")?;
        write_upperdir_file(&workspace, "src/main.rs", b"source")?;
        write_upperdir_sparse_file(&workspace, "ignored/huge.bin", (16 * 1024 * 1024) + 1)?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_ignored_limit_source_publish".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(wire["changed_paths"], serde_json::json!(["src/main.rs"]));
        assert_eq!(
            wire["publish_lanes"]["source"]["publish_status"],
            "committed"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_status"],
            "dropped_due_to_limits"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["drop_reason"],
            layerstack::service::IGNORED_FILE_BYTE_LIMIT_DROP_REASON
        );
        assert_eq!(wire["publish_lanes"]["ignored"]["path_count"], 1);
        assert_eq!(
            wire["publish_lanes"]["ignored"]["bytes"],
            serde_json::json!((16 * 1024 * 1024) + 1)
        );
        assert_eq!(wire["publish_lanes"]["ignored"]["spooled_bytes"], 0);
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("src/main.rs")?
                .0,
            "source"
        );
        let (_bytes, ignored_exists) =
            LayerStack::open(fixture.root.clone())?.read_bytes("ignored/huge.bin")?;
        assert!(
            !ignored_exists,
            "limit-dropped ignored payload must not publish"
        );
        assert!(
            !workspace
                .dirs()
                .run_dir
                .join("spool")
                .join("publish-capture")
                .exists(),
            "limit-dropped ignored payload must not leave a spool directory"
        );
        Ok(())
    }

    #[test]
    fn source_conflict_drops_ignored_output_and_reports_lanes() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("source-conflict-drops-ignored")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
            path: layerstack::LayerPath::parse("src/main.rs")?,
            content: b"theirs".to_vec(),
        }])?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "source-conflict-ignored")?;
        write_upperdir_file(&workspace, "src/main.rs", b"mine")?;
        write_upperdir_file(&workspace, "ignored/cache.txt", b"ignored")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_source_conflict_drops_ignored".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], false);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(
            wire["publish_lanes"]["source"]["publish_status"],
            "conflict"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_status"],
            "dropped_due_to_source_conflict"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["drop_reason"],
            "source_not_published"
        );
        assert_eq!(wire["publish_lanes"]["source"]["path_count"], 1);
        assert_eq!(wire["publish_lanes"]["ignored"]["path_count"], 1);
        assert_eq!(wire["publish_lanes"]["ignored"]["bytes"], 7);
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("src/main.rs")?
                .0,
            "theirs"
        );
        let (_bytes, ignored_exists) =
            LayerStack::open(fixture.root.clone())?.read_bytes("ignored/cache.txt")?;
        assert!(
            !ignored_exists,
            "ignored payload must not publish when source lane conflicts"
        );
        Ok(())
    }

    #[test]
    fn source_and_ignored_success_reports_combined_lane_publish() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("source-ignored-success")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "source-ignored-success")?;
        write_upperdir_file(&workspace, "src/main.rs", b"source")?;
        write_upperdir_file(&workspace, "ignored/cache.txt", b"ignored")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_source_ignored_success".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        let active_version = LayerStack::open(fixture.root.clone())?
            .read_active_manifest()?
            .version;
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(active_version, snapshot.manifest_version + 1);
        assert_eq!(
            wire["publish_lanes"]["source"]["publish_status"],
            "committed"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_status"],
            "published_lww"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_mode"],
            "direct_lww"
        );
        assert_eq!(wire["publish_lanes"]["source"]["path_count"], 1);
        assert_eq!(wire["publish_lanes"]["ignored"]["path_count"], 1);
        assert_eq!(wire["publish_lanes"]["ignored"]["bytes"], 7);
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("src/main.rs")?
                .0,
            "source"
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("ignored/cache.txt")?
                .0,
            "ignored"
        );
        Ok(())
    }

    #[test]
    fn source_conflict_takes_precedence_over_ignored_limit_drop_status() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("ignored-limit-source-conflict")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
            path: layerstack::LayerPath::parse("src/main.rs")?,
            content: b"theirs".to_vec(),
        }])?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "ignored-limit-conflict")?;
        write_upperdir_file(&workspace, "src/main.rs", b"mine")?;
        write_upperdir_sparse_file(&workspace, "ignored/huge.bin", (16 * 1024 * 1024) + 1)?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_ignored_limit_source_conflict".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], false);
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_eq!(
            wire["publish_lanes"]["source"]["publish_status"],
            "conflict"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_status"],
            "dropped_due_to_source_conflict"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["drop_reason"],
            "source_not_published"
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_text("src/main.rs")?
                .0,
            "theirs"
        );
        let (_bytes, ignored_exists) =
            LayerStack::open(fixture.root.clone())?.read_bytes("ignored/huge.bin")?;
        assert!(
            !ignored_exists,
            "ignored payload must not publish when source lane conflicts"
        );
        Ok(())
    }

    #[test]
    fn successful_ephemeral_command_reports_spooled_ignored_bytes_and_cleans_spool() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("ignored-spooled-success")?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace =
            EphemeralWorkspace::create(&fixture.scratch, "command", "ignored-spooled-success")?;
        let payload = vec![b'x'; (1024 * 1024) + 1];
        write_upperdir_file(&workspace, "ignored/large.bin", &payload)?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_ignored_spooled_success".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], true);
        assert_eq!(
            wire["publish_lanes"]["ignored"]["publish_status"],
            "published_lww"
        );
        assert_eq!(
            wire["publish_lanes"]["ignored"]["spooled_bytes"],
            serde_json::json!((1024 * 1024) + 1)
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_bytes("ignored/large.bin")?
                .0
                .expect("published ignored payload")
                .len(),
            (1024 * 1024) + 1
        );
        assert!(
            !workspace
                .dirs()
                .run_dir
                .join("spool")
                .join("publish-capture")
                .exists(),
            "spool directory should be removed after successful publish"
        );
        Ok(())
    }

    #[test]
    fn failed_publish_cleans_spooled_ignored_payloads() -> TestResult {
        let fixture = EphemeralFinalizeFixture::new("ignored-spool-publish-failure")?;
        LayerStack::open(fixture.root.clone())?.publish_layer(&[
            LayerChange::Write {
                path: layerstack::LayerPath::parse("tree/src.txt")?,
                content: b"source".to_vec(),
            },
            LayerChange::Write {
                path: layerstack::LayerPath::parse("tree/ignored/cache.txt")?,
                content: b"ignored".to_vec(),
            },
        ])?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace = EphemeralWorkspace::create(
            &fixture.scratch,
            "command",
            "ignored-spool-publish-failure",
        )?;
        let payload = vec![b'y'; (1024 * 1024) + 1];
        write_upperdir_file(&workspace, "ignored/large.bin", &payload)?;
        write_upperdir_opaque_marker(&workspace, "tree")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status: CommandStatus::Ok,
                exit_code: Some(0),
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_ignored_spool_publish_failure".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["success"], false);
        assert_eq!(
            wire["publish_lanes"]["routing"]["drop_reason_counts"],
            serde_json::json!({"opaque_dir_mixed_routes": 1})
        );
        assert_eq!(wire["publish_lanes"]["ignored"]["publish_status"], "failed");
        assert_eq!(
            wire["publish_lanes"]["ignored"]["spooled_bytes"],
            serde_json::json!((1024 * 1024) + 1)
        );
        assert!(
            !workspace
                .dirs()
                .run_dir
                .join("spool")
                .join("publish-capture")
                .exists(),
            "spool directory should be removed after publish failure"
        );
        Ok(())
    }

    fn assert_non_success_discards(status: CommandStatus, exit_code: Option<i64>) -> TestResult {
        let fixture = EphemeralFinalizeFixture::new(status.as_str())?;
        let snapshot = service::acquire_snapshot(&fixture.root, "test-command")?;
        let workspace = EphemeralWorkspace::create(&fixture.scratch, "command", status.as_str())?;
        write_upperdir_file(&workspace, "src/main.rs", b"source")?;
        write_upperdir_file(&workspace, "ignored/cache.txt", b"ignored")?;

        let response = finalize_ephemeral_command(
            &fixture.root,
            &snapshot,
            &workspace,
            CommitOptions::default(),
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 0.25,
                status,
                exit_code,
                stdout: "stdout".to_owned(),
                stderr: String::new(),
                command_id: Some("cmd_non_success".to_owned()),
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        let wire = response.to_wire_value();
        assert_eq!(wire["status"], status.as_str());
        assert_eq!(wire["changed_paths"], serde_json::json!([]));
        assert_lane_statuses_dropped(&wire["publish_lanes"]);
        assert_eq!(
            wire["publish_lanes"]["routing"]["route_manifest_version"],
            snapshot.manifest_version
        );
        assert_eq!(
            LayerStack::open(fixture.root.clone())?
                .read_active_manifest()?
                .version,
            snapshot.manifest_version,
            "non-success command must not advance the manifest"
        );
        for path in ["src/main.rs", "ignored/cache.txt"] {
            let (_bytes, exists) = LayerStack::open(fixture.root.clone())?.read_bytes(path)?;
            assert!(!exists, "{path} must not publish for {status:?}");
        }
        Ok(())
    }

    fn assert_lane_statuses_dropped(publish_lanes: &Value) {
        assert_eq!(
            publish_lanes["source"]["publish_status"],
            "dropped_command_failed"
        );
        assert_eq!(
            publish_lanes["ignored"]["publish_status"],
            "dropped_command_failed"
        );
        assert_eq!(publish_lanes["source"]["path_count"], 1);
        assert_eq!(publish_lanes["ignored"]["path_count"], 1);
        assert_eq!(publish_lanes["ignored"]["bytes"], 7);
        assert_eq!(
            publish_lanes["routing"]["ignore_route_source"],
            "command_snapshot"
        );
    }

    fn write_upperdir_file(
        workspace: &EphemeralWorkspace,
        path: &str,
        content: &[u8],
    ) -> std::io::Result<()> {
        let path = workspace.dirs().upperdir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    fn write_upperdir_sparse_file(
        workspace: &EphemeralWorkspace,
        path: &str,
        len: u64,
    ) -> std::io::Result<()> {
        let path = workspace.dirs().upperdir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        file.set_len(len)
    }

    fn write_upperdir_opaque_marker(
        workspace: &EphemeralWorkspace,
        path: &str,
    ) -> std::io::Result<()> {
        let marker = workspace.dirs().upperdir.join(path).join(".wh..wh..opq");
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(marker, b"")
    }

    struct EphemeralFinalizeFixture {
        base: std::path::PathBuf,
        root: std::path::PathBuf,
        scratch: std::path::PathBuf,
    }

    impl EphemeralFinalizeFixture {
        fn new(label: &str) -> TestResult<Self> {
            let base = std::env::temp_dir().join(format!(
                "operation-finalize-{label}-{}-{}",
                std::process::id(),
                unix_nanos()
            ));
            let root = base.join("layer-stack");
            let layer = root.join("layers").join("B000001-base");
            let scratch = base.join("scratch");
            std::fs::create_dir_all(&layer)?;
            std::fs::create_dir_all(root.join("staging"))?;
            std::fs::create_dir_all(&scratch)?;
            std::fs::write(layer.join(".gitignore"), "ignored/\n")?;
            std::fs::write(
                root.join("manifest.json"),
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "version": 1,
                    "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
                }))?,
            )?;
            Ok(Self {
                base,
                root,
                scratch,
            })
        }
    }

    impl Drop for EphemeralFinalizeFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn unix_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    }
}

//! Oneshot plugin overlay execution.
//!
//! Write-capable plugin routes that ask for an automatic workspace overlay run
//! here: the plugin module builds the command, this module runs the ns-runner,
//! captures the upperdir, and publishes through the daemon's shared OCC path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eos_layerstack::{require_workspace_binding, LayerStack, Lease, WorkspaceBinding};
use eos_overlay::{allocate_overlay_writable_dirs, capture_upperdir, overlay_writable_root, OverlayWritableDirs};
use eos_protocol::{Intent, LayerChange};
use eos_runner::{RunMode, RunRequest, RunResult, ToolCall, WorkspaceRoot};
use serde_json::{json, Value};

use crate::dispatcher::{
    apply_occ_changeset, attach_runner_shell_fields, base_hashes_for_snapshot,
    guarded_changeset_response, insert_occ_route_timings, insert_tree_resource_timings,
    layer_change_kind, manifest_version_u64, merge_runner_timings, occ_route_metrics,
    resource_timings, run_ns_runner_child, TreeResourceStats,
};
use crate::error::DaemonError;

pub(crate) struct PluginOverlayCommand {
    pub(crate) layer_stack_root: PathBuf,
    pub(crate) invocation_id: String,
    pub(crate) agent_id: String,
    pub(crate) public_op: String,
    pub(crate) plugin_id: String,
    pub(crate) op_name: String,
    pub(crate) command: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) timeout_seconds: Option<f64>,
}

struct PluginOverlayRunOutcome {
    runner: RunResult,
    changeset: eos_occ::ChangesetResult,
    plugin_result: Option<Value>,
    path_kinds: Vec<(String, String)>,
    route_metrics: crate::dispatcher::OccRouteMetrics,
    route_s: f64,
    capture_s: f64,
    occ_s: f64,
    upperdir_stats: TreeResourceStats,
}

struct RunDirCleanup(PathBuf);

impl Drop for RunDirCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn run_plugin_overlay_command(
    spec: &PluginOverlayCommand,
    args: &Value,
    total_start: Instant,
) -> Result<Value, DaemonError> {
    if spec.command.is_empty() || spec.command[0].trim().is_empty() {
        return Err(DaemonError::InvalidEnvelope(
            "plugin overlay command must not be empty".to_owned(),
        ));
    }
    let binding = require_workspace_binding(&spec.layer_stack_root)?;
    let mut stack = LayerStack::open(spec.layer_stack_root.clone())?;
    let acquire_start = Instant::now();
    let lease = stack.acquire_snapshot(&format!(
        "plugin-overlay:{}:{}",
        spec.agent_id, spec.invocation_id
    ))?;
    let lease_acquire_s = acquire_start.elapsed().as_secs_f64();
    let run_result = run_plugin_overlay_once(spec, args, &binding, &lease);
    let _ = stack.release_lease(&lease.lease_id);
    let outcome = run_result?;
    plugin_overlay_response(
        &spec.layer_stack_root,
        outcome,
        total_start,
        lease_acquire_s,
    )
}

fn run_plugin_overlay_once(
    spec: &PluginOverlayCommand,
    args: &Value,
    binding: &WorkspaceBinding,
    lease: &Lease,
) -> Result<PluginOverlayRunOutcome, DaemonError> {
    let dirs = plugin_overlay_dirs(&spec.invocation_id)?;
    let _cleanup = RunDirCleanup(dirs.run_dir.clone());
    let request_path = dirs.run_dir.join("plugin-overlay-request.json");
    let result_path = dirs.run_dir.join("plugin-overlay-result.json");
    write_plugin_overlay_request(spec, args, binding, lease, &request_path, &result_path)?;

    let request =
        plugin_overlay_run_request(spec, binding, lease, &dirs, &request_path, &result_path);
    let runner = run_ns_runner_child(&request, None)?;
    let plugin_result = read_plugin_overlay_result(&result_path)?;
    let (changes, path_kinds, capture_s) = capture_upperdir_for_occ(&dirs.upperdir)?;
    let upperdir_stats = TreeResourceStats::collect(&dirs.upperdir);
    let route_start = Instant::now();
    let route_metrics = occ_route_metrics(&spec.layer_stack_root, &changes)?;
    let route_s = route_start.elapsed().as_secs_f64();
    let base_hashes = base_hashes_for_snapshot(&spec.layer_stack_root, &lease.manifest, &changes)?;
    let occ_start = Instant::now();
    let changeset = apply_occ_changeset(
        &spec.layer_stack_root,
        Some(manifest_version_u64(lease.manifest_version)?),
        &changes,
        &base_hashes,
    )?;
    let occ_s = occ_start.elapsed().as_secs_f64();
    Ok(PluginOverlayRunOutcome {
        runner,
        changeset,
        plugin_result,
        path_kinds,
        route_metrics,
        route_s,
        capture_s,
        occ_s,
        upperdir_stats,
    })
}

fn plugin_overlay_response(
    root: &Path,
    outcome: PluginOverlayRunOutcome,
    total_start: Instant,
    lease_acquire_s: f64,
) -> Result<Value, DaemonError> {
    let manifest = LayerStack::open(root.to_path_buf())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, published_file_count(&outcome.changeset));
    merge_runner_timings(&mut timings, &outcome.runner);
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &outcome.upperdir_stats,
    );
    timings.insert(
        "layer_stack.acquire_snapshot.total_s".to_owned(),
        json!(lease_acquire_s),
    );
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(outcome.capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(outcome.occ_s));
    timings.insert(
        "command_exec.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    insert_occ_route_timings(
        &mut timings,
        outcome.route_metrics,
        outcome.route_s,
        outcome.occ_s,
    );
    let mut response = guarded_changeset_response(
        "plugin_overlay",
        &outcome.changeset,
        timings,
        total_start,
        None,
    );
    attach_runner_shell_fields(&mut response, &outcome.runner);
    response["changed_path_kinds"] = Value::Object(
        outcome
            .path_kinds
            .iter()
            .map(|(path, kind)| (path.clone(), json!(kind)))
            .collect(),
    );
    let worker_success = outcome
        .plugin_result
        .as_ref()
        .and_then(|result| result.get("success"))
        .and_then(Value::as_bool);
    response["plugin_result"] = outcome.plugin_result.unwrap_or_else(|| json!({}));
    response["plugin_overlay"] = json!({
        "changed_paths": outcome
            .path_kinds
            .iter()
            .map(|(path, _kind)| path.clone())
            .collect::<Vec<_>>(),
        "published_manifest_version": outcome.changeset.published_manifest_version,
        "worker_exit_code": outcome.runner.exit_code,
    });
    apply_plugin_overlay_status(
        &mut response,
        outcome.runner.exit_code,
        outcome.changeset.success(),
        worker_success,
    );
    Ok(response)
}

fn apply_plugin_overlay_status(
    response: &mut Value,
    worker_exit_code: i32,
    changeset_success: bool,
    worker_success: Option<bool>,
) {
    if worker_exit_code != 0 {
        response["success"] = json!(false);
        response["status"] = json!("failed");
        response["error"] = json!({
            "kind": "plugin_overlay_worker_failed",
            "message": "plugin overlay worker exited with a non-zero status",
        });
    } else if changeset_success && response["conflict"].is_null() {
        if worker_success == Some(false) {
            response["success"] = json!(false);
            response["status"] = json!("failed");
            response["error"] = json!({
                "kind": "plugin_overlay_worker_failed",
                "message": "plugin overlay worker reported failure",
            });
        } else {
            response["success"] = json!(true);
            response["status"] = json!("committed");
        }
    }
}

fn plugin_overlay_dirs(invocation_id: &str) -> Result<OverlayWritableDirs, DaemonError> {
    let run_root = overlay_writable_root()
        .map_err(|err| overlay_daemon_error("overlay writable root", &err))?
        .join("runtime")
        .join("plugin-overlay")
        .join(format!(
            "{}-{}",
            std::process::id(),
            sanitize_path_component(invocation_id)
        ));
    allocate_overlay_writable_dirs(&run_root)
        .map_err(|err| overlay_daemon_error("allocate overlay dirs", &err))
}

fn write_plugin_overlay_request(
    spec: &PluginOverlayCommand,
    args: &Value,
    binding: &WorkspaceBinding,
    lease: &Lease,
    request_path: &Path,
    result_path: &Path,
) -> Result<(), DaemonError> {
    let request_payload = json!({
        "plugin": spec.plugin_id,
        "op_name": spec.op_name,
        "public_op": spec.public_op,
        "args": args,
        "layer_stack_root": &spec.layer_stack_root,
        "workspace_root": &binding.workspace_root,
        "manifest_version": lease.manifest_version,
        "manifest_root_hash": lease.root_hash,
        "request_path": request_path,
        "result_path": result_path,
    });
    std::fs::write(
        request_path,
        serde_json::to_vec(&request_payload)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    Ok(())
}

fn plugin_overlay_run_request(
    spec: &PluginOverlayCommand,
    binding: &WorkspaceBinding,
    lease: &Lease,
    dirs: &OverlayWritableDirs,
    request_path: &Path,
    result_path: &Path,
) -> RunRequest {
    let mut env = spec.env.clone();
    env.insert("EOS_PLUGIN_OPERATION".to_owned(), spec.public_op.clone());
    env.insert("EOS_PLUGIN_OP_NAME".to_owned(), spec.op_name.clone());
    env.insert(
        "EOS_PLUGIN_INVOCATION_ID".to_owned(),
        spec.invocation_id.clone(),
    );
    env.insert(
        "EOS_PLUGIN_REQUEST_PATH".to_owned(),
        request_path.to_string_lossy().into_owned(),
    );
    env.insert(
        "EOS_PLUGIN_RESULT_PATH".to_owned(),
        result_path.to_string_lossy().into_owned(),
    );
    RunRequest {
        mode: RunMode::FreshNs,
        tool_call: ToolCall {
            invocation_id: spec.invocation_id.clone(),
            agent_id: spec.agent_id.clone(),
            verb: "plugin_service".to_owned(),
            intent: Intent::WriteAllowed,
            args: json!({
                "command": spec.command.clone(),
                "cwd": ".",
                "env": env,
            }),
            background: false,
        },
        workspace_root: WorkspaceRoot(PathBuf::from(&binding.workspace_root)),
        layer_paths: lease.layer_paths.iter().map(PathBuf::from).collect(),
        upperdir: Some(dirs.upperdir.clone()),
        workdir: Some(dirs.workdir.clone()),
        ns_fds: None,
        cgroup_path: None,
        timeout_seconds: spec.timeout_seconds,
    }
}

type CapturedOverlayChanges = (Vec<LayerChange>, Vec<(String, String)>, f64);

fn capture_upperdir_for_occ(upperdir: &Path) -> Result<CapturedOverlayChanges, DaemonError> {
    let capture_start = Instant::now();
    let changes =
        capture_upperdir(upperdir).map_err(|err| overlay_daemon_error("capture upperdir", &err))?;
    let capture_s = capture_start.elapsed().as_secs_f64();
    let path_kinds = changes
        .iter()
        .map(|change| {
            (
                change.path().as_str().to_owned(),
                layer_change_kind(change).to_owned(),
            )
        })
        .collect();
    Ok((changes, path_kinds, capture_s))
}

fn read_plugin_overlay_result(path: &Path) -> Result<Option<Value>, DaemonError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            if raw.trim().is_empty() {
                Ok(None)
            } else {
                serde_json::from_str(&raw)
                    .map(Some)
                    .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn published_file_count(result: &eos_occ::ChangesetResult) -> usize {
    result
        .files
        .iter()
        .filter(|file| file.status.is_published())
        .count()
}

fn overlay_daemon_error(context: &str, err: &eos_overlay::OverlayError) -> DaemonError {
    DaemonError::OverlayPipeline(format!("{context}: {err}"))
}

fn sanitize_path_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "request".to_owned()
    } else {
        cleaned
    }
}

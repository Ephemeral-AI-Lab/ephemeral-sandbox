//! Oneshot plugin overlay execution.
//!
//! Write-capable plugin routes that ask for an automatic workspace overlay run
//! here: the plugin module builds the command, this module runs the ns-runner,
//! captures the upperdir, and publishes through the daemon's shared OCC path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eos_ephemeral_workspace::{
    capture_upperdir, overlay_run_dirs, path_changes_to_wire, OverlayDirs, OverlayDirsGuard,
};
use eos_layerstack::{require_workspace_binding, LayerStack, Lease, WorkspaceBinding};
use eos_namespace::protocol::Intent;
use eos_namespace::protocol::{RunMode, RunRequest, RunResult, ToolCall, WorkspaceRoot};
use eos_plugin::ServiceMode;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::response::{
    attach_runner_shell_fields, guarded_changeset_response, insert_tree_resource_timings,
    merge_runner_timings, resource_timings, u64_to_f64_saturating, TreeResourceStats,
};
use crate::runtime::ns_runner::run_ns_runner_child;

use eos_plugin::host::route::PluginOperationRoute;

struct PluginOverlayCommand {
    layer_stack_root: PathBuf,
    invocation_id: String,
    caller_id: String,
    public_op: String,
    plugin_id: String,
    op_name: String,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    timeout_seconds: Option<f64>,
}

pub(super) fn dispatch_oneshot_overlay_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Option<Value>, DaemonError> {
    if route.service_mode != Some(ServiceMode::OneshotOverlay) {
        return Ok(None);
    }
    let Some(layer_stack_root) = route.layer_stack_root.clone() else {
        return Ok(None);
    };
    let Some(service_key) = route.service_key.clone() else {
        return Ok(None);
    };
    if route.service_command.is_empty() {
        return Ok(None);
    }
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_owned();
    let mut env = BTreeMap::from([
        (
            "EOS_PLUGIN_LAYER_STACK_ROOT".to_owned(),
            service_key.layer_stack_root,
        ),
        (
            "EOS_PLUGIN_WORKSPACE_ROOT".to_owned(),
            service_key.workspace_root,
        ),
        ("EOS_PLUGIN_ID".to_owned(), service_key.plugin_id),
        ("EOS_PLUGIN_DIGEST".to_owned(), service_key.plugin_digest),
        ("EOS_PLUGIN_SERVICE_ID".to_owned(), service_key.service_id),
        (
            "EOS_PLUGIN_SERVICE_PROFILE_DIGEST".to_owned(),
            service_key.service_profile_digest,
        ),
        (
            "EOS_PLUGIN_PPC_PROTOCOL_VERSION".to_owned(),
            route.service_ppc_protocol_version.unwrap_or(1).to_string(),
        ),
        (
            "EOS_PLUGIN_SERVICE_MODE".to_owned(),
            "oneshot_overlay".to_owned(),
        ),
    ]);
    env.insert("EOS_PLUGIN_PUBLIC_OP".to_owned(), route.public_op.clone());
    let timeout_seconds = route
        .timeout_ms
        .map(|timeout| u64_to_f64_saturating(timeout) / 1000.0);
    let overlay_command = PluginOverlayCommand {
        layer_stack_root: PathBuf::from(layer_stack_root),
        invocation_id: invocation_id.to_owned(),
        caller_id,
        public_op: route.public_op.clone(),
        plugin_id: route.plugin_id.clone(),
        op_name: route.op_name.clone(),
        command: route.service_command.clone(),
        env,
        timeout_seconds,
    };
    Ok(Some(run_plugin_overlay_command(
        &overlay_command,
        args,
        Instant::now(),
    )?))
}

struct PluginOverlayRunOutcome {
    runner: RunResult,
    changeset: eos_layerstack::ChangesetResult,
    plugin_result: Option<Value>,
    path_kinds: Vec<(String, String)>,
    capture_s: f64,
    occ_s: f64,
    upperdir_stats: TreeResourceStats,
}

fn run_plugin_overlay_command(
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
        spec.caller_id, spec.invocation_id
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
    let _cleanup = OverlayDirsGuard::new(dirs.run_dir.clone());
    let request_path = dirs.run_dir.join("plugin-overlay-request.json");
    let result_path = dirs.run_dir.join("plugin-overlay-result.json");
    write_plugin_overlay_request(spec, args, binding, lease, &request_path, &result_path)?;

    let request =
        plugin_overlay_run_request(spec, binding, lease, &dirs, &request_path, &result_path);
    let runner = run_ns_runner_child(&request, None)?;
    let plugin_result = read_plugin_overlay_result(&result_path)?;
    let captured = capture_upperdir(&dirs.upperdir)
        .map_err(|err| DaemonError::OverlayPipeline(err.to_string()))?;
    let publish_start = Instant::now();
    let layer_paths: Vec<PathBuf> = lease.layer_paths.iter().map(PathBuf::from).collect();
    let changeset = eos_layerstack::service::publish_capture(
        &spec.layer_stack_root,
        lease.manifest_version,
        &layer_paths,
        &captured.changes,
    )?;
    let publish_s = publish_start.elapsed().as_secs_f64();
    let path_kinds = path_changes_to_wire(&captured.path_kinds);
    let upperdir_stats = TreeResourceStats::from_ephemeral(&captured.stats);
    let capture_s = captured.capture_s;
    let occ_s = changeset
        .timings
        .get("occ.commit.total_s")
        .copied()
        .unwrap_or(publish_s);
    Ok(PluginOverlayRunOutcome {
        runner,
        changeset,
        plugin_result,
        path_kinds,
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
    let mut timings = resource_timings(&manifest, outcome.changeset.published_file_count());
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

fn plugin_overlay_dirs(invocation_id: &str) -> Result<OverlayDirs, DaemonError> {
    overlay_run_dirs("plugin-overlay", invocation_id)
        .map_err(|err| DaemonError::OverlayPipeline(err.to_string()))
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
    dirs: &OverlayDirs,
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
            caller_id: spec.caller_id.clone(),
            verb: "plugin_service".into(),
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

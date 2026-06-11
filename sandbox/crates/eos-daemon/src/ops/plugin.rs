//! Plugin op adapters: wire arg parsing, the caller-family gate, response
//! shaping over the typed [`crate::services::plugin`] outcomes, and the
//! resource-telemetry splice for oneshot overlay runs.

use std::time::Instant;

use eos_layerstack::LayerStack;
use eos_plugin::PluginError;
use eos_plugin_runtime::ensure::validate_plugin_caller_fields;
use eos_plugin_runtime::needs_upload_response;
use eos_plugin_runtime::route::{PluginOperationRoute, PluginProcessSpec};
use eos_plugin_runtime::PackageEnsureReport;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::response::{
    attach_runner_shell_fields, guarded_changeset_response, insert_tree_resource_timings,
    merge_runner_timings, resource_timings,
};
use crate::runtime::context::DispatchContext;
use crate::runtime::services::Services;
use crate::services::plugin::{
    EnsureOutcome, EnsureReady, LoadedPluginStatus, PluginDispatchOutcome, PluginOverlayOutcome,
    ServiceHealthReport, StatusOutcome,
};

pub(crate) fn op_ensure(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    ensure_plugin_family_allowed(services, args)?;
    let start_services = args
        .get("start_services")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match services.plugin.ensure(args, start_services)? {
        EnsureOutcome::NeedsUpload { manifest, report } => {
            Ok(needs_upload_response(&manifest, &report))
        }
        EnsureOutcome::Ready(ready) => Ok(ensure_response(&ready)),
    }
}

pub(crate) fn op_status(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    ensure_plugin_family_allowed(services, args)?;
    let probe_services = args
        .get("probe_services")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let probe_timeout = args
        .get("probe_timeout_ms")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_millis);
    let outcome = services.plugin.status(probe_services, probe_timeout)?;
    Ok(status_response(&outcome))
}

/// Dispatch a dynamically registered `plugin.*` op after a built-in table miss,
/// or `None` when the op is not plugin-shaped / not registered.
pub(crate) fn dispatch_registered_op(
    op: &str,
    invocation_id: &str,
    args: &Value,
    context: DispatchContext<'_>,
) -> Option<Result<Value, DaemonError>> {
    if !op.starts_with("plugin.") {
        return None;
    }
    let services = match context.require_services() {
        Ok(services) => services,
        Err(err) => return Some(Err(err)),
    };
    // Single caller-family gate for the whole registered-op dispatch chain; the
    // routing below it trusts the already-validated args.
    if let Err(err) = ensure_plugin_family_allowed(services, args) {
        return Some(Err(err));
    }
    let total_start = Instant::now();
    let outcome = services.plugin.dispatch_registered_op(op, invocation_id, args)?;
    Some(outcome.and_then(|outcome| match outcome {
        PluginDispatchOutcome::Response(response) => Ok(response),
        PluginDispatchOutcome::OneshotOverlay(overlay) => {
            plugin_overlay_response(&overlay, total_start)
        }
    }))
}

/// The plugin caller-family gate: validate the caller fields, then refuse the
/// whole `api.plugin.*` + registered-op family for callers inside an isolated
/// workspace. Composed here so the plugin runtime never reaches into
/// isolated-workspace state.
fn ensure_plugin_family_allowed(services: &Services, args: &Value) -> Result<(), DaemonError> {
    validate_plugin_caller_fields(args).map_err(DaemonError::from)?;
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !caller_id.is_empty() && services.workspace.caller_has_active_handle(caller_id) {
        return Err(DaemonError::Plugin(
            PluginError::ForbiddenInIsolatedWorkspace,
        ));
    }
    Ok(())
}

fn ensure_response(ready: &EnsureReady) -> Value {
    json!({
        "success": true,
        "plugin": ready.plugin_id,
        "digest": ready.digest,
        "registered_ops": ready.registered_ops,
        "runtime_loaded": ready.runtime_loaded,
        "runtime_warmed": false,
        "service_processes_started": ready.started_count > 0,
        "started_service_process_count": ready.started_count,
        "already_loaded": ready.already_loaded,
        "operation_routes": route_values(&ready.operation_routes),
        "services": ready.services,
        "service_processes": process_values(&ready.service_processes),
        "running_service_processes": ready.running_service_processes,
        "connected_ppc_routes": ready.connected_ppc_routes,
        "connected_ppc_services": ready.connected_ppc_services,
        "package": package_report_value(&ready.package),
    })
}

fn status_response(outcome: &StatusOutcome) -> Value {
    json!({
        "success": true,
        "loaded_plugins": outcome
            .loaded_plugins
            .iter()
            .map(loaded_plugin_value)
            .collect::<Vec<_>>(),
        "running_service_processes": outcome.running_service_processes,
        "connected_ppc_routes": outcome.connected_ppc_routes,
        "connected_ppc_services": outcome.connected_ppc_services,
        "setup_failures": outcome.setup_failures,
        "service_health": outcome
            .service_health
            .iter()
            .map(service_health_value)
            .collect::<Vec<_>>(),
        "pending": [],
    })
}

fn loaded_plugin_value(loaded: &LoadedPluginStatus) -> Value {
    json!({
        "name": loaded.name,
        "digest": loaded.digest,
        "ops": loaded.ops,
        "operation_routes": route_values(&loaded.operation_routes),
        "services": loaded.services,
        "service_processes": process_values(&loaded.service_processes),
        "runtime_loaded": loaded.runtime_loaded,
    })
}

fn service_health_value(health: &ServiceHealthReport) -> Value {
    if health.success {
        json!({
            "success": true,
            "plugin": health.plugin,
            "service_id": health.service_id,
            "service_instance_id": health.service_instance_id,
            "manifest_key": health.manifest_key,
            "accepted": health.accepted,
        })
    } else {
        json!({
            "success": false,
            "plugin": health.plugin,
            "service_id": health.service_id,
            "service_instance_id": health.service_instance_id,
            "manifest_key": health.manifest_key,
            "error": health.error,
            "teardown_error": health.teardown_error,
        })
    }
}

fn route_values(routes: &[PluginOperationRoute]) -> Vec<Value> {
    routes.iter().map(route_to_json).collect()
}

/// Wire-shape one resolved route. Hand-rolled (not a serde derive) so the key
/// set and order stay byte-stable independent of the runtime crate's fields.
fn route_to_json(route: &PluginOperationRoute) -> Value {
    json!({
        "plugin": route.plugin_id,
        "op_name": route.op_name,
        "public_op": route.public_op,
        "layer_stack_root": route.layer_stack_root,
        "intent": route.intent,
        "auto_workspace_overlay": route.auto_workspace_overlay,
        "service_id": route.service_id,
        "service_instance_id": route.service_instance_id,
        "service_mode": route.service_mode,
        "service_command": route.service_command,
        "timeout_ms": route.timeout_ms,
        "dispatch_mode": route.dispatch_mode(),
    })
}

fn process_values(processes: &[PluginProcessSpec]) -> Vec<Value> {
    processes.iter().map(process_spec_to_json).collect()
}

fn process_spec_to_json(spec: &PluginProcessSpec) -> Value {
    json!({
        "service_id": spec.key.service_id,
        "service_instance_id": spec.key.service_instance_id(),
        "command": spec.command,
        "package_root": spec.package_root,
        "dependency_root": spec.dependency_root,
        "working_dir": spec.working_dir,
        "socket_path": spec.socket_path,
        "env": spec.environment(),
        "ppc_protocol_version": spec.ppc_protocol_version,
        "process_started": false,
    })
}

fn package_report_value(report: &PackageEnsureReport) -> Value {
    if !report.active {
        return Value::Null;
    }
    json!({
        "needs_upload": report.needs_upload,
        "package_root": report.package_root.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "dependency_root": report.dependency_root.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "package_published": report.package_published,
        "setup_ran": report.setup_ran,
    })
}

/// Shape the oneshot overlay wire response: the guarded changeset shape plus
/// runner shell fields, plugin worker result, and the daemon's latest-state
/// resource telemetry sample.
fn plugin_overlay_response(
    overlay: &PluginOverlayOutcome,
    total_start: Instant,
) -> Result<Value, DaemonError> {
    let manifest = LayerStack::open(overlay.layer_stack_root.clone())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, overlay.changeset.published_file_count());
    merge_runner_timings(&mut timings, &overlay.runner);
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &overlay.upperdir_stats,
    );
    timings.insert(
        "layer_stack.acquire_snapshot.total_s".to_owned(),
        json!(overlay.lease_acquire_s),
    );
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(overlay.capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(overlay.occ_s));
    timings.insert(
        "command_exec.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    let mut response = guarded_changeset_response(
        "plugin_overlay",
        &overlay.changeset,
        timings,
        total_start,
        None,
    );
    attach_runner_shell_fields(&mut response, &overlay.runner);
    response["changed_path_kinds"] = Value::Object(
        overlay
            .path_kinds
            .iter()
            .map(|(path, kind)| (path.clone(), json!(kind)))
            .collect(),
    );
    let worker_success = overlay
        .plugin_result
        .as_ref()
        .and_then(|result| result.get("success"))
        .and_then(Value::as_bool);
    response["plugin_result"] = overlay.plugin_result.clone().unwrap_or_else(|| json!({}));
    response["plugin_overlay"] = json!({
        "changed_paths": overlay
            .path_kinds
            .iter()
            .map(|(path, _kind)| path.clone())
            .collect::<Vec<_>>(),
        "published_manifest_version": overlay.changeset.published_manifest_version,
        "worker_exit_code": overlay.runner.exit_code,
    });
    apply_plugin_overlay_status(
        &mut response,
        overlay.runner.exit_code,
        overlay.changeset.success(),
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

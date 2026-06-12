//! Plugin op adapters: wire arg parsing, response shaping over the typed
//! `eos_operation::plugin` outcomes, and the
//! resource-telemetry splice for oneshot overlay runs.

use std::time::Instant;

use eos_layerstack::LayerStack;
use eos_operation::plugin::contract::{
    LoadedPluginStatusOutput, PluginEnsureInput, PluginEnsureOutput, PluginEnsureReadyOutput,
    PluginStatusInput, PluginStatusOutput,
};
use eos_operation::plugin::needs_upload_output;
use eos_operation::plugin::route::{PluginOperationRoute, PluginProcessSpec};
use eos_operation::plugin::PackageEnsureReport;
use serde_json::{json, Map, Value};

use crate::error::DaemonError;
use crate::response::{
    attach_runner_shell_fields, insert_cgroup_process_resource_timings,
    insert_tree_resource_timings, merge_runner_timings, plugin_overlay_changeset_response,
    resource_timings, TreeResourceStats,
};
use crate::DispatchContext;
use eos_operation::plugin::{
    EnsureOutcome, EnsureReady, LoadedPluginStatus, PluginDispatchOutcome, PluginOverlayOutcome,
    PluginRuntimeError, PluginSetupReport, PpcError, PpcTraceEvent, ServiceHealthReport,
    ServiceProcessStatus, StatusOutcome,
};

use super::to_wire_value;

pub(crate) fn op_ensure(
    input: PluginEnsureInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    let ensure_result = services.plugin.ensure(&input);
    if let Err(err) = &ensure_result {
        record_plugin_ensure_error_trace_events(&context, err);
    }
    let output = match ensure_result.map_err(DaemonError::from)? {
        EnsureOutcome::NeedsUpload { manifest, report } => {
            PluginEnsureOutput::NeedsUpload(needs_upload_output(&manifest, &report))
        }
        EnsureOutcome::Ready(ready) => {
            record_plugin_ensure_trace_events(&context, &ready);
            PluginEnsureOutput::Ready(Box::new(ensure_ready_output(&ready)))
        }
    };
    Ok(to_wire_value(output))
}

pub(crate) fn op_status(
    input: PluginStatusInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    let probe_timeout = input.probe_timeout_ms.map(std::time::Duration::from_millis);
    let outcome = services
        .plugin
        .status(input.probe_services, probe_timeout)
        .map_err(DaemonError::from)?;
    record_plugin_status_trace_events(&context, &outcome);
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
    if let Err(err) = services.ensure_plugin_family_allowed(args) {
        return Some(Err(DaemonError::from(err)));
    }
    let total_start = Instant::now();
    let mut before_resource_timings = Map::new();
    insert_cgroup_process_resource_timings(&mut before_resource_timings);
    push_stale_ppc_background_root(services.plugin.drain_ppc_trace_events());
    let outcome = services
        .plugin
        .dispatch_registered_op(op, invocation_id, args)?;
    let result = outcome
        .map_err(DaemonError::from)
        .and_then(|outcome| match outcome {
            PluginDispatchOutcome::Response(response) => Ok(response),
            PluginDispatchOutcome::OneshotOverlay(overlay) => {
                context.record_trace_event(
                    "workspace.route",
                    "route_selected",
                    json!({
                        "kind": "ephemeral_workspace",
                        "reason": "plugin_oneshot_overlay",
                    }),
                );
                plugin_overlay_response_with_trace(
                    &context,
                    op,
                    invocation_id,
                    &overlay,
                    &before_resource_timings,
                    total_start,
                )
            }
        });
    record_ppc_trace_events(&context, services.plugin.drain_ppc_trace_events());
    Some(result)
}

fn ensure_ready_output(ready: &EnsureReady) -> PluginEnsureReadyOutput {
    PluginEnsureReadyOutput {
        success: true,
        plugin: ready.plugin_id.clone(),
        digest: ready.digest.clone(),
        registered_ops: ready.registered_ops.clone(),
        runtime_loaded: ready.runtime_loaded,
        runtime_warmed: false,
        service_processes_started: ready.started_count > 0,
        started_service_process_count: ready.started_count,
        already_loaded: ready.already_loaded,
        operation_routes: route_values(&ready.operation_routes),
        services: to_wire_value(&ready.services),
        service_processes: process_values(&ready.service_processes),
        running_service_processes: to_wire_value(&ready.running_service_processes),
        connected_ppc_routes: ready.connected_ppc_routes.clone(),
        connected_ppc_services: ready.connected_ppc_services.clone(),
        package: package_report_value(&ready.package),
    }
}

fn status_response(outcome: &StatusOutcome) -> Value {
    to_wire_value(PluginStatusOutput {
        success: true,
        loaded_plugins: outcome
            .loaded_plugins
            .iter()
            .map(loaded_plugin_value)
            .collect::<Vec<_>>(),
        running_service_processes: to_wire_value(&outcome.running_service_processes),
        connected_ppc_routes: outcome.connected_ppc_routes.clone(),
        connected_ppc_services: outcome.connected_ppc_services.clone(),
        setup_failures: to_wire_value(&outcome.setup_failures),
        service_health: outcome
            .service_health
            .iter()
            .map(service_health_value)
            .collect::<Vec<_>>(),
        pending: Vec::new(),
    })
}

fn loaded_plugin_value(loaded: &LoadedPluginStatus) -> LoadedPluginStatusOutput {
    LoadedPluginStatusOutput {
        name: loaded.name.clone(),
        digest: loaded.digest.clone(),
        ops: loaded.ops.clone(),
        operation_routes: route_values(&loaded.operation_routes),
        services: to_wire_value(&loaded.services),
        service_processes: process_values(&loaded.service_processes),
        runtime_loaded: loaded.runtime_loaded,
    }
}

fn service_health_value(health: &ServiceHealthReport) -> Value {
    if health.success {
        json!({
            "success": true,
            "plugin": health.plugin,
            "service_id": health.service_id,
            "service_instance_id": health.service_instance_id,
            "manifest_key": health.manifest_key,
            "state": health.state,
            "restart_count": health.restart_count,
            "refresh_count": health.refresh_count,
            "last_error": health.last_error,
            "accepted": health.accepted,
        })
    } else {
        json!({
            "success": false,
            "plugin": health.plugin,
            "service_id": health.service_id,
            "service_instance_id": health.service_instance_id,
            "manifest_key": health.manifest_key,
            "state": health.state,
            "restart_count": health.restart_count,
            "refresh_count": health.refresh_count,
            "last_error": health.last_error,
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
        "stderr_path": spec.stderr_path,
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
        &TreeResourceStats::from_ephemeral(&overlay.upperdir_stats),
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
    let mut response = plugin_overlay_changeset_response(&overlay.changeset, timings, total_start);
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

fn plugin_overlay_response_with_trace(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
    before_resource_timings: &Map<String, Value>,
    total_start: Instant,
) -> Result<Value, DaemonError> {
    record_plugin_overlay_resource_stats(context, "before", before_resource_timings);
    record_plugin_overlay_host_resource_stats(context, "before", before_resource_timings);
    record_plugin_overlay_started(context, op, invocation_id, overlay);
    record_plugin_overlay_mount_finished(context, op, invocation_id, overlay);
    record_plugin_overlay_unmount_finished(context, op, invocation_id, overlay);
    record_plugin_overlay_capture_started(context, op, invocation_id, overlay);
    let result = plugin_overlay_response(overlay, total_start);
    if let Ok(response) = &result {
        if let Some(timings) = response.get("timings").and_then(Value::as_object) {
            record_plugin_overlay_resource_stats(context, "after", timings);
            record_plugin_overlay_host_resource_stats(context, "after", timings);
        }
    }
    record_plugin_overlay_capture_finished(context, op, invocation_id, overlay);
    record_occ_changeset_trace_events(context, &overlay.changeset);
    match &result {
        Ok(response) => {
            record_plugin_overlay_finished(context, op, invocation_id, overlay, response, None)
        }
        Err(err) => record_plugin_overlay_finished(
            context,
            op,
            invocation_id,
            overlay,
            &json!({ "success": false, "status": "error" }),
            Some(err),
        ),
    }
    result
}

fn record_plugin_overlay_started(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
) {
    context.record_trace_event(
        "plugin",
        "overlay_started",
        json!({
            "op": op,
            "invocation_id": invocation_id,
            "layer_stack_root": overlay.layer_stack_root,
        }),
    );
}

fn record_plugin_overlay_resource_stats(
    context: &DispatchContext<'_>,
    phase: &'static str,
    timings: &Map<String, Value>,
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
                "source": "plugin.overlay.run",
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

fn record_plugin_overlay_host_resource_stats(
    context: &DispatchContext<'_>,
    phase: &'static str,
    timings: &Map<String, Value>,
) {
    let mut process = Map::new();
    for (key, value) in timings {
        if let Some(name) = key.strip_prefix("resource.process.") {
            process.insert(name.to_owned(), value.clone());
        }
    }
    let source_available = !process.is_empty();
    context.record_trace_event(
        "resource",
        "resource_stats",
        json!({
            "meta": {
                "stats_kind": "host",
                "phase": phase,
                "source": "daemon.process",
                "source_available": source_available,
                "read_error": (!source_available).then_some("daemon process gauges unavailable on this platform"),
                "sampler_duration_us": 0,
                "inflight_requests": context
                    .invocation_registry()
                    .map_or(0, crate::invocation_registry::InFlightRegistry::inflight_count),
            },
            "host": {
                "process": process,
            },
        }),
    );
}

fn record_plugin_overlay_mount_finished(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
) {
    let mount_s = runner_timing(&overlay.runner, "workspace.mount_s");
    let fsconfig_calls = runner_timing(&overlay.runner, "workspace.fsconfig_calls");
    let duration_us = mount_s.map(seconds_to_micros_saturating);
    context.record_trace_event(
        "overlay",
        "mount_finished",
        json!({
            "op": op,
            "invocation_id": invocation_id,
            "source": "plugin_oneshot_overlay",
            "layer_stack_root": overlay.layer_stack_root,
            "success": mount_s.is_some(),
            "duration_s": mount_s,
            "duration_available": mount_s.is_some(),
            "layer_count": overlay.layer_count,
            "fsconfig_calls": fsconfig_calls,
            "fsconfig_calls_available": fsconfig_calls.is_some(),
        }),
    );
    context.record_trace_event(
        "resource",
        "resource_stats",
        json!({
            "meta": {
                "stats_kind": "mount_cost",
                "phase": "after",
                "source": "plugin.overlay.mount",
                "source_available": mount_s.is_some(),
                "read_error": mount_s.is_none().then_some("overlay mount timing unavailable"),
                "sampler_duration_us": 0,
                "inflight_requests": context
                    .invocation_registry()
                    .map_or(0, crate::invocation_registry::InFlightRegistry::inflight_count),
            },
            "mount": {
                "duration_us": duration_us,
                "duration_available": duration_us.is_some(),
                "layer_count": overlay.layer_count,
                "fsconfig_calls": fsconfig_calls,
                "fsconfig_calls_available": fsconfig_calls.is_some(),
            },
        }),
    );
}

fn record_plugin_overlay_unmount_finished(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
) {
    let unmount_s = runner_timing(&overlay.runner, "workspace.unmount_s");
    let unmount_error = overlay
        .runner
        .payload
        .get("workspace_unmount_error")
        .and_then(Value::as_str);
    context.record_trace_event(
        "overlay",
        "unmount_finished",
        json!({
            "op": op,
            "invocation_id": invocation_id,
            "source": "plugin_oneshot_overlay",
            "layer_stack_root": overlay.layer_stack_root,
            "success": unmount_s.is_some() && unmount_error.is_none(),
            "duration_s": unmount_s,
            "duration_available": unmount_s.is_some(),
            "layer_count": overlay.layer_count,
            "error": unmount_error,
        }),
    );
}

fn runner_timing(runner: &eos_namespace::protocol::RunResult, key: &str) -> Option<f64> {
    runner
        .payload
        .get("timings")
        .and_then(Value::as_object)
        .and_then(|timings| timings.get(key))
        .and_then(Value::as_f64)
}

fn seconds_to_micros_saturating(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    let micros = seconds * 1_000_000.0;
    if micros >= u64::MAX as f64 {
        u64::MAX
    } else {
        micros.round() as u64
    }
}

fn record_plugin_overlay_capture_started(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
) {
    context.record_trace_event(
        "overlay",
        "capture_started",
        json!({
            "op": op,
            "invocation_id": invocation_id,
            "source": "plugin_oneshot_overlay",
            "layer_stack_root": overlay.layer_stack_root,
        }),
    );
}

fn record_plugin_overlay_capture_finished(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
) {
    context.record_trace_event(
        "overlay",
        "capture_finished",
        json!({
            "op": op,
            "invocation_id": invocation_id,
            "source": "plugin_oneshot_overlay",
            "layer_stack_root": overlay.layer_stack_root,
            "success": true,
            "duration_s": overlay.capture_s,
            "changed_path_count": overlay.path_kinds.len(),
            "bytes": overlay.upperdir_stats.bytes,
            "file_count": overlay.upperdir_stats.files,
            "dir_count": overlay.upperdir_stats.dirs,
            "symlink_count": overlay.upperdir_stats.symlinks,
            "entry_count": overlay
                .upperdir_stats
                .files
                .saturating_add(overlay.upperdir_stats.dirs)
                .saturating_add(overlay.upperdir_stats.symlinks),
            "truncated": overlay.upperdir_stats.truncated,
            "read_error_count": overlay.upperdir_stats.read_error_count,
            "failing_path": overlay.upperdir_stats.first_error_path.clone(),
        }),
    );
}

fn record_occ_changeset_trace_events(
    context: &DispatchContext<'_>,
    changeset: &eos_layerstack::ChangesetResult,
) {
    for event in changeset.trace_events() {
        context.record_trace_event(event.module, event.name, event.details);
    }
}

fn record_ppc_trace_events(context: &DispatchContext<'_>, events: Vec<PpcTraceEvent>) {
    for event in events {
        context.record_trace_event(event.module, event.name, event.details);
    }
}

/// PPC facts that accumulated with no plugin op in flight (orphan replies,
/// refused callbacks) become a standalone `PluginService` background root
/// instead of being dropped or misattributed to the next request's trace.
fn push_stale_ppc_background_root(events: Vec<PpcTraceEvent>) {
    use eos_trace::{EventRecord, SpanKind, SpanRecord, SpanUid, TraceId, TraceKind, TraceRecord};
    if events.is_empty() {
        return;
    }
    let now = crate::trace::now_ms();
    let mut span = SpanRecord::new(
        SpanUid::ROOT,
        None,
        "plugin.service",
        SpanKind::Plugin,
        json!({"event_count": events.len(), "source": "stale_ppc_drain"}),
    );
    span.started_at_unix_ms = now;
    span.finished_at_unix_ms = now;
    let mut record = TraceRecord::new(TraceId::new(), SpanUid::ROOT);
    record.kind = TraceKind::PluginService;
    record.started_at_unix_ms = now;
    record.finished_at_unix_ms = now;
    record.spans.push(span);
    for event in events {
        let mut event_record =
            EventRecord::new(SpanUid::ROOT, event.name, event.module, event.details);
        event_record.at_unix_ms = now;
        record.events.push(event_record);
    }
    crate::trace::push_background_record(record);
}

fn record_plugin_overlay_finished(
    context: &DispatchContext<'_>,
    op: &str,
    invocation_id: &str,
    overlay: &PluginOverlayOutcome,
    response: &Value,
    adapter_error: Option<&DaemonError>,
) {
    context.record_trace_event(
        "plugin",
        "overlay_finished",
        json!({
            "op": op,
            "invocation_id": invocation_id,
            "layer_stack_root": overlay.layer_stack_root,
            "success": response.get("success").and_then(Value::as_bool).unwrap_or(false),
            "status": response.get("status").and_then(Value::as_str),
            "error_kind": response
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str),
            "adapter_error": adapter_error.map(ToString::to_string),
            "worker_exit_code": overlay.runner.exit_code,
            "changed_path_count": overlay.path_kinds.len(),
            "published_manifest_version": overlay.changeset.published_manifest_version,
            "lease_acquire_s": overlay.lease_acquire_s,
            "capture_s": overlay.capture_s,
            "occ_s": overlay.occ_s,
            "upperdir_files": overlay.upperdir_stats.files,
            "upperdir_dirs": overlay.upperdir_stats.dirs,
            "upperdir_symlinks": overlay.upperdir_stats.symlinks,
            "upperdir_bytes": overlay.upperdir_stats.bytes,
        }),
    );
}

fn record_plugin_ensure_trace_events(context: &DispatchContext<'_>, ready: &EnsureReady) {
    if let Some(setup) = &ready.package.setup {
        record_plugin_setup_finished(context, setup);
    }
    for process in &ready.started_service_processes {
        context.record_trace_event(
            "plugin",
            "service_started",
            json!({
                "plugin": ready.plugin_id,
                "service_id": process.service_id,
                "service_instance_id": process.service_instance_id,
                "pid": process.pid,
                "process_group_id": process.process_group_id,
                "running": process.running,
                "socket_path": process.socket_path,
                "stderr_path": process.stderr_path,
            }),
        );
    }
}

fn record_plugin_ensure_error_trace_events(
    context: &DispatchContext<'_>,
    err: &PluginRuntimeError,
) {
    if let PluginRuntimeError::Ppc(PpcError::SetupFailed { report, .. }) = err {
        record_plugin_setup_finished(context, report);
    }
}

fn record_plugin_setup_finished(context: &DispatchContext<'_>, report: &PluginSetupReport) {
    context.record_trace_event(
        "plugin",
        "setup_finished",
        json!({
            "plugin": report.plugin,
            "digest": report.digest,
            "ran": report.ran,
            "success": report.success,
            "exit_code": report.exit_code,
            "output_tail": report.output_tail,
            "spawn_error": report.spawn_error,
        }),
    );
}

fn record_plugin_status_trace_events(context: &DispatchContext<'_>, outcome: &StatusOutcome) {
    for health in &outcome.service_health {
        context.record_trace_event(
            "plugin",
            "service_health_checked",
            json!({
                "plugin": health.plugin,
                "service_id": health.service_id,
                "service_instance_id": health.service_instance_id,
                "manifest_key": health.manifest_key,
                "state": health.state,
                "restart_count": health.restart_count,
                "refresh_count": health.refresh_count,
                "last_error": health.last_error,
                "accepted": health.accepted,
                "success": health.success,
                "error": health.error,
                "teardown_error": health.teardown_error,
            }),
        );
    }
    for process in &outcome.exited_service_processes {
        record_service_exited(context, process);
    }
    for process in outcome
        .running_service_processes
        .iter()
        .filter(|process| !process.running)
    {
        record_service_exited(context, process);
    }
}

fn record_service_exited(context: &DispatchContext<'_>, process: &ServiceProcessStatus) {
    context.record_trace_event(
        "plugin",
        "service_exited",
        json!({
            "service_id": process.service_id,
            "service_instance_id": process.service_instance_id,
            "pid": process.pid,
            "process_group_id": process.process_group_id,
            "exit_code": process.exit_status,
            "signal": process.exit_signal,
            "status_raw": process.status_raw,
            "socket_path": process.socket_path,
            "stderr_path": process.stderr_path,
        }),
    );
}

#[cfg(test)]
#[path = "../../tests/unit/plugin/mod.rs"]
mod tests;

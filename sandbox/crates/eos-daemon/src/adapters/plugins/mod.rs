//! Daemon plugin API surface.
//!
//! This module owns the daemon-side `api.plugin.*` facade. It keeps the
//! contract-only `eos-plugin` crate free of sandbox publish edges while sibling
//! modules own service process lifetime, PPC dispatch, manifest refresh,
//! plugin-originated OCC callbacks, and oneshot overlay execution.

mod connected;
mod dispatch;
mod occ_callbacks;
mod overlay;
mod process;
mod refresh;
mod service;
mod setup;
mod state;

#[cfg(test)]
use std::sync::Arc;
use std::time::Duration;

use eos_plugin::PluginError;
#[cfg(test)]
use eos_plugin::PluginServiceState;
#[cfg(test)]
use eos_plugin::{PpcDirection, PpcEnvelope};
use serde_json::{json, Value};

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
#[cfg(test)]
use connected::response_payload_from_reply;
#[cfg(test)]
use dispatch::route_for_op;
#[cfg(test)]
use eos_plugin::host::ensure_args::validate_plugin_caller_fields;
use eos_plugin::host::ensure_args::ParsedEnsure;
#[cfg(test)]
use eos_plugin::host::ensure_args::MAX_PLUGIN_CALLER_FIELD_CHARS;
use eos_plugin::host::{ensure_package, needs_upload_response};
#[cfg(test)]
use refresh::WORKSPACE_SNAPSHOT_REFRESH_OP;
use refresh::{probe_service_health, service_health_probe_targets};
#[cfg(test)]
use service::{
    acquire_service_snapshot, active_manifest_key, mark_service_ready, release_service_snapshot,
    service_status_mut,
};
use service::{
    insert_started_service_processes, reap_exited_processes, running_process_values,
    service_specs_to_start, spawn_service_processes, stop_plugin_service_processes,
};
pub(crate) use setup::{configure_plugin_runtime, stop_services_for_layer_stack_root};
use setup::{
    ensure_plugin_family_allowed, package_report_value, plugin_runtime_config, ppc_socket_root,
    record_setup_failure,
};
use state::loaded_matches_parsed;
use state::{
    connected_ppc_routes, connected_ppc_services, loaded_plugin_values, lock_state, process_values,
    route_values, setup_failure_key, setup_failure_values, LoadedPluginRuntime,
};

pub(crate) fn op_ensure(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    ensure_plugin_family_allowed(args)?;

    let parsed = ParsedEnsure::from_args(args, &ppc_socket_root(args))?;
    let start_services = args
        .get("start_services")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let package_report = match ensure_package(args, parsed.manifest.as_ref()) {
        Ok(report) => report,
        Err(err) => {
            let err = DaemonError::from(err);
            record_setup_failure(parsed.manifest.as_ref(), &err);
            return Err(err);
        }
    };
    if package_report.needs_upload {
        let manifest = parsed.manifest.as_ref().ok_or_else(|| {
            DaemonError::Plugin(PluginError::Ensure(
                "package ensure requested upload without manifest".to_owned(),
            ))
        })?;
        return Ok(needs_upload_response(manifest, &package_report));
    }
    let (already_loaded, specs_to_start) = {
        let mut state = lock_state()?;
        if package_report.active {
            state
                .setup_failures
                .remove(&setup_failure_key(&parsed.plugin_id, &parsed.plugin_digest));
        }
        let already_loaded = state
            .loaded
            .get(&parsed.plugin_id)
            .is_some_and(|loaded| loaded_matches_parsed(loaded, &parsed));
        if !already_loaded {
            stop_plugin_service_processes(&mut state, &parsed.plugin_id);
            state.loaded.insert(
                parsed.plugin_id.clone(),
                LoadedPluginRuntime {
                    digest: parsed.plugin_digest.clone(),
                    registered_ops: parsed.registered_ops.clone(),
                    operation_routes: parsed.operation_routes.clone(),
                    services: parsed.services.clone(),
                    service_processes: parsed.service_processes.clone(),
                    runtime_loaded: parsed.runtime_loaded,
                },
            );
        }
        let process_specs = state
            .loaded
            .get(&parsed.plugin_id)
            .ok_or_else(|| {
                DaemonError::Plugin(PluginError::Ensure(format!(
                    "plugin {} was not recorded after ensure",
                    parsed.plugin_id
                )))
            })?
            .service_processes
            .clone();
        let specs_to_start = if start_services {
            service_specs_to_start(&state, &process_specs)
        } else {
            Vec::new()
        };
        drop(state);
        (already_loaded, specs_to_start)
    };
    let started_services = spawn_service_processes(&specs_to_start)?;
    let mut state = lock_state()?;
    let started_count = insert_started_service_processes(&mut state, started_services)?;
    let loaded = state.loaded.get(&parsed.plugin_id).ok_or_else(|| {
        DaemonError::Plugin(PluginError::Ensure(format!(
            "plugin {} was not recorded after ensure",
            parsed.plugin_id
        )))
    })?;
    let digest = loaded.digest.clone();
    let registered_ops = loaded.registered_ops.clone();
    let runtime_loaded = loaded.runtime_loaded;
    let operation_routes = route_values(&loaded.operation_routes);
    let services = loaded.services.clone();
    let service_processes = process_values(&loaded.service_processes);
    let running_service_processes = running_process_values(&mut state);
    let connected_ppc_routes = connected_ppc_routes(&state);
    let connected_ppc_services = connected_ppc_services(&state);
    drop(state);

    Ok(json!({
        "success": true,
        "plugin": parsed.plugin_id,
        "digest": digest,
        "registered_ops": registered_ops,
        "runtime_loaded": runtime_loaded,
        "runtime_warmed": false,
        "service_processes_started": started_count > 0,
        "started_service_process_count": started_count,
        "already_loaded": already_loaded,
        "operation_routes": operation_routes,
        "services": services,
        "service_processes": service_processes,
        "running_service_processes": running_service_processes,
        "connected_ppc_routes": connected_ppc_routes,
        "connected_ppc_services": connected_ppc_services,
        "package": package_report_value(&package_report),
    }))
}

pub(crate) fn op_status(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    ensure_plugin_family_allowed(args)?;
    let probe_services = args
        .get("probe_services")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let probe_timeout = Duration::from_millis(
        args.get("probe_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| plugin_runtime_config().service_probe_timeout_ms),
    );
    let probe_targets = {
        let mut state = lock_state()?;
        reap_exited_processes(&mut state);
        if probe_services {
            service_health_probe_targets(&state)
        } else {
            Vec::new()
        }
    };
    let service_health = probe_service_health(probe_targets, probe_timeout);
    let mut state = lock_state()?;
    let running_service_processes = running_process_values(&mut state);
    let loaded_plugins = loaded_plugin_values(&state);
    let connected_ppc_routes = connected_ppc_routes(&state);
    let connected_ppc_services = connected_ppc_services(&state);
    let setup_failures = setup_failure_values(&state);
    drop(state);
    Ok(json!({
        "success": true,
        "loaded_plugins": loaded_plugins,
        "running_service_processes": running_service_processes,
        "connected_ppc_routes": connected_ppc_routes,
        "connected_ppc_services": connected_ppc_services,
        "setup_failures": setup_failures,
        "service_health": service_health,
        "pending": [],
    }))
}

pub(crate) fn dispatch_registered_op(
    op: &str,
    invocation_id: &str,
    args: &Value,
    context: DispatchContext<'_>,
) -> Option<Result<Value, DaemonError>> {
    dispatch::dispatch_registered_op(op, invocation_id, args, context)
}

#[cfg(test)]
fn reset_for_tests() {
    for snapshot in state::reset_state_for_tests() {
        release_service_snapshot(&snapshot);
    }
}

#[cfg(test)]
fn register_ppc_client_for_tests(
    op: &str,
    stream: std::os::unix::net::UnixStream,
) -> Result<(), DaemonError> {
    let mut state = lock_state()?;
    let (service_instance_id, manifest_key) = state
        .loaded
        .values()
        .find_map(|loaded| loaded.operation_routes.get(op))
        .map_or_else(
            || (op.to_owned(), None),
            |route| {
                let manifest_key = route
                    .service_key
                    .as_ref()
                    .and_then(|key| active_manifest_key(&key.layer_stack_root).ok());
                (
                    route
                        .service_instance_id
                        .clone()
                        .unwrap_or_else(|| op.to_owned()),
                    manifest_key,
                )
            },
        );
    if let Some(manifest_key) = manifest_key {
        if let Ok(status) = service_status_mut(&mut state, &service_instance_id) {
            status.state = PluginServiceState::Ready;
            status.manifest_key = Some(manifest_key);
            status.last_error = None;
        }
    }
    state.service_ppc_clients.insert(
        service_instance_id,
        Arc::new(eos_plugin::host::PpcClient::new(stream)?),
    );
    drop(state);
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/plugin/mod.rs"]
mod tests;

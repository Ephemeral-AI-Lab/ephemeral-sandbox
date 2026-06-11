//! Connected plugin service freshness, refresh, and teardown.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eos_plugin::{
    PluginError, PluginServiceKey, PluginServiceState, PpcDirection, PpcEnvelope, RefreshAck,
    RefreshRequest, RefreshStrategy, ServiceMode,
};
use serde_json::{json, Value};

use super::{
    plugin_runtime_config, process,
    service::{
        acquire_service_snapshot, active_manifest_key, insert_started_service_processes,
        mark_service_ready, mark_service_restarted, mark_service_stale, mark_service_stopped,
        release_service_snapshot, service_status_mut, spawn_service_processes,
        PluginServiceSnapshot,
    },
    state::{find_service_status, lock_state, DaemonPluginState, SharedPpcClient},
};
use crate::error::DaemonError;

use eos_plugin::host::route::PluginOperationRoute;

pub(super) const WORKSPACE_SNAPSHOT_REFRESH_OP: &str = "daemon.workspace_snapshot_refresh";

#[derive(Debug, Clone)]
pub(super) struct ServiceHealthProbeTarget {
    plugin_id: String,
    service_id: String,
    service_instance_id: String,
    manifest_key: String,
    client: SharedPpcClient,
}

pub(super) fn service_health_probe_targets(
    state: &DaemonPluginState,
) -> Vec<ServiceHealthProbeTarget> {
    state
        .loaded
        .values()
        .flat_map(|loaded| loaded.services.iter())
        .filter_map(|status| {
            let service_instance_id = status.key.service_instance_id();
            let client = state.service_ppc_clients.get(&service_instance_id)?;
            let snapshot = state.service_snapshots.get(&service_instance_id)?;
            Some(ServiceHealthProbeTarget {
                plugin_id: status.key.plugin_id.clone(),
                service_id: status.key.service_id.clone(),
                service_instance_id,
                manifest_key: snapshot.manifest_key.clone(),
                client: Arc::clone(client),
            })
        })
        .collect()
}

pub(super) fn probe_service_health(
    targets: Vec<ServiceHealthProbeTarget>,
    timeout: Duration,
) -> Vec<Value> {
    targets
        .into_iter()
        .enumerate()
        .map(
            |(index, target)| match probe_connected_service_health(&target, index, timeout) {
                Ok(health) => health,
                Err(err) => {
                    let error = err.to_string();
                    let teardown_error =
                        teardown_failed_connected_service(&target.service_instance_id, &error)
                            .err()
                            .map(|err| err.to_string());
                    json!({
                        "success": false,
                        "plugin": target.plugin_id,
                        "service_id": target.service_id,
                        "service_instance_id": target.service_instance_id,
                        "manifest_key": target.manifest_key,
                        "error": error,
                        "teardown_error": teardown_error,
                    })
                }
            },
        )
        .collect()
}

fn probe_connected_service_health(
    target: &ServiceHealthProbeTarget,
    index: usize,
    timeout: Duration,
) -> Result<Value, DaemonError> {
    let request = RefreshRequest::Health {
        manifest_key: target.manifest_key.clone(),
    };
    let envelope = PpcEnvelope {
        message_id: format!("api.plugin.status:health:{index}"),
        direction: PpcDirection::Request,
        op: WORKSPACE_SNAPSHOT_REFRESH_OP.to_owned(),
        body: serde_json::to_string(&request).map_err(|err| PluginError::Ppc(err.to_string()))?,
    };
    let reply = target.client.round_trip(&envelope, timeout)?;
    let ack: RefreshAck =
        serde_json::from_str(&reply.body).map_err(|err| PluginError::Ppc(err.to_string()))?;
    ack.require_manifest(&target.manifest_key)?;
    Ok(json!({
        "success": true,
        "plugin": target.plugin_id,
        "service_id": target.service_id,
        "service_instance_id": target.service_instance_id,
        "manifest_key": target.manifest_key,
        "accepted": ack.accepted,
    }))
}

pub(super) fn ensure_connected_service_current(
    route: &PluginOperationRoute,
    invocation_id: &str,
) -> Result<Option<SharedPpcClient>, DaemonError> {
    let Some(service_instance_id) = route.service_instance_id.as_deref() else {
        return Ok(None);
    };
    ensure_tracked_service_process_running(service_instance_id)?;
    let Some(service_key) = route.service_key.as_ref() else {
        let Some(client) = ppc_client_for_service(service_instance_id)? else {
            return Ok(None);
        };
        return Ok(Some(client));
    };
    if route.service_mode != Some(ServiceMode::WorkspaceSnapshotRefresh) {
        let Some(client) = ppc_client_for_service(service_instance_id)? else {
            return Ok(None);
        };
        return Ok(Some(client));
    }

    if let Some(client) = ppc_client_for_service(service_instance_id)? {
        let target_manifest_key = active_manifest_key(&service_key.layer_stack_root)?;
        if service_is_ready_on_manifest(service_instance_id, &target_manifest_key)? {
            return Ok(Some(client));
        }
    } else if !service_was_started_before(service_instance_id)? {
        return Ok(None);
    }

    let refresh_lock = refresh_lock_for_service(service_instance_id)?;
    let _refresh_guard = refresh_lock
        .lock()
        .map_err(|_| DaemonError::StateLockPoisoned("plugin service refresh"))?;
    ensure_tracked_service_process_running(service_instance_id)?;
    let Some(client) = ppc_client_for_service(service_instance_id)? else {
        if service_was_started_before(service_instance_id)? {
            return restart_read_only_service(service_instance_id);
        }
        return Ok(None);
    };
    let target_manifest_key = active_manifest_key(&service_key.layer_stack_root)?;
    if service_is_ready_on_manifest(service_instance_id, &target_manifest_key)? {
        return Ok(Some(client));
    }
    if service_key.refresh_strategy == RefreshStrategy::RestartService {
        return restart_read_only_service(service_instance_id);
    }

    refresh_connected_service(
        route,
        service_key,
        service_instance_id,
        &client,
        invocation_id,
    )?;
    Ok(Some(client))
}

fn refresh_lock_for_service(service_instance_id: &str) -> Result<Arc<Mutex<()>>, DaemonError> {
    let mut state = lock_state()?;
    Ok(state
        .service_refresh_locks
        .entry(service_instance_id.to_owned())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

fn service_was_started_before(service_instance_id: &str) -> Result<bool, DaemonError> {
    let state = lock_state()?;
    Ok(find_service_status(&state, service_instance_id)
        .is_some_and(|status| status.manifest_key.is_some()))
}

fn service_is_ready_on_manifest(
    service_instance_id: &str,
    target_manifest_key: &str,
) -> Result<bool, DaemonError> {
    let state = lock_state()?;
    Ok(
        find_service_status(&state, service_instance_id).is_some_and(|status| {
            status
                .require_ready_on_manifest(target_manifest_key)
                .is_ok()
        }),
    )
}

fn refresh_connected_service(
    route: &PluginOperationRoute,
    service_key: &PluginServiceKey,
    service_instance_id: &str,
    client: &SharedPpcClient,
    invocation_id: &str,
) -> Result<(), DaemonError> {
    let snapshot = acquire_service_snapshot(service_key, "refresh")?;
    let timeout = Duration::from_millis(
        route
            .timeout_ms
            .unwrap_or_else(|| plugin_runtime_config().ppc_timeout_ms),
    );
    let refresh_result = send_refresh_sequence(
        client,
        service_key,
        service_instance_id,
        invocation_id,
        &snapshot,
        timeout,
    );
    if let Err(err) = refresh_result {
        release_service_snapshot(&snapshot);
        let mut state = lock_state()?;
        let _ = mark_service_stale(&mut state, service_instance_id, err.to_string());
        return Err(err);
    }

    let old_snapshot = {
        let mut state = lock_state()?;
        mark_service_ready(&mut state, service_instance_id, &snapshot, true)?;
        state
            .service_snapshots
            .insert(service_instance_id.to_owned(), snapshot)
    };
    if let Some(old_snapshot) = old_snapshot {
        release_service_snapshot(&old_snapshot);
    }
    Ok(())
}

fn send_refresh_sequence(
    client: &eos_plugin::host::PpcClient,
    service_key: &PluginServiceKey,
    service_instance_id: &str,
    invocation_id: &str,
    snapshot: &PluginServiceSnapshot,
    timeout: Duration,
) -> Result<(), DaemonError> {
    let request_id = format!("{invocation_id}:refresh");
    send_refresh_request(
        client,
        invocation_id,
        0,
        &RefreshRequest::PrepareRefresh {
            target_manifest_key: snapshot.manifest_key.clone(),
        },
        snapshot,
        timeout,
    )?;
    send_refresh_request(
        client,
        invocation_id,
        1,
        &RefreshRequest::Quiesce {
            request_id: request_id.clone(),
        },
        snapshot,
        timeout,
    )?;
    remount_connected_service_workspace(service_instance_id, service_key, snapshot, timeout)?;

    let mut requests = vec![RefreshRequest::SwapWorkspace {
        layer_paths: snapshot.layer_paths.clone(),
        workspace_root: service_key.workspace_root.clone(),
        manifest_key: snapshot.manifest_key.clone(),
    }];
    if service_key.refresh_strategy == RefreshStrategy::RemountWorkspaceAndNotify {
        requests.push(RefreshRequest::NotifyRefresh {
            changed_paths: Vec::new(),
            full_resync: true,
        });
    }
    requests.push(RefreshRequest::Resume { request_id });
    requests.push(RefreshRequest::Health {
        manifest_key: snapshot.manifest_key.clone(),
    });

    for (index, request) in requests.iter().enumerate() {
        send_refresh_request(client, invocation_id, index + 2, request, snapshot, timeout)?;
    }
    Ok(())
}

fn remount_connected_service_workspace(
    service_instance_id: &str,
    service_key: &PluginServiceKey,
    snapshot: &PluginServiceSnapshot,
    timeout: Duration,
) -> Result<(), DaemonError> {
    let Some(overlay) = snapshot.overlay.as_ref() else {
        return Ok(());
    };
    let target_pid = service_process_pid(service_instance_id)?;
    process::remount_workspace_overlay(target_pid, &service_key.workspace_root, overlay, timeout)
}

fn service_process_pid(service_instance_id: &str) -> Result<u32, DaemonError> {
    let pid = {
        let mut state = lock_state()?;
        let process = state
            .service_processes
            .get_mut(service_instance_id)
            .ok_or_else(|| {
                DaemonError::Plugin(PluginError::Ensure(format!(
                    "service {service_instance_id} process is not running for workspace remount"
                )))
            })?;
        if process.status_json()["running"] != true {
            return Err(DaemonError::Plugin(PluginError::Ensure(format!(
                "service {service_instance_id} process exited before workspace remount"
            ))));
        }
        let pid = process.pid();
        drop(state);
        pid
    };
    Ok(pid)
}

fn send_refresh_request(
    client: &eos_plugin::host::PpcClient,
    invocation_id: &str,
    index: usize,
    request: &RefreshRequest,
    snapshot: &PluginServiceSnapshot,
    timeout: Duration,
) -> Result<(), DaemonError> {
    let envelope = PpcEnvelope {
        message_id: format!("{invocation_id}:refresh:{index}"),
        direction: PpcDirection::Request,
        op: WORKSPACE_SNAPSHOT_REFRESH_OP.to_owned(),
        body: serde_json::to_string(&request).map_err(|err| PluginError::Ppc(err.to_string()))?,
    };
    let reply = client.round_trip(&envelope, timeout)?;
    let ack: RefreshAck =
        serde_json::from_str(&reply.body).map_err(|err| PluginError::Ppc(err.to_string()))?;
    ack.require_manifest(&snapshot.manifest_key)?;
    Ok(())
}

fn restart_read_only_service(
    service_instance_id: &str,
) -> Result<Option<SharedPpcClient>, DaemonError> {
    let (spec, old_snapshot) = {
        let mut state = lock_state()?;
        let spec = state
            .loaded
            .values()
            .flat_map(|loaded| loaded.service_processes.iter())
            .find(|spec| spec.service_instance_id() == service_instance_id)
            .cloned();
        state.service_processes.remove(service_instance_id);
        state.service_ppc_clients.remove(service_instance_id);
        (spec, state.service_snapshots.remove(service_instance_id))
    };
    let Some(spec) = spec else {
        return Ok(None);
    };
    if let Some(old_snapshot) = old_snapshot {
        release_service_snapshot(&old_snapshot);
    }
    let started = spawn_service_processes(&[spec])?;
    let mut state = lock_state()?;
    insert_started_service_processes(&mut state, started)?;
    mark_service_restarted(&mut state, service_instance_id)?;
    Ok(state.service_ppc_clients.get(service_instance_id).cloned())
}

fn ppc_client_for_service(
    service_instance_id: &str,
) -> Result<Option<SharedPpcClient>, DaemonError> {
    Ok(lock_state()?
        .service_ppc_clients
        .get(service_instance_id)
        .cloned())
}

fn ensure_tracked_service_process_running(service_instance_id: &str) -> Result<(), DaemonError> {
    let snapshot_to_release = {
        let mut state = lock_state()?;
        let Some(process) = state.service_processes.get_mut(service_instance_id) else {
            return Ok(());
        };
        if process.status_json()["running"] == true {
            return Ok(());
        }
        state.service_processes.remove(service_instance_id);
        state.service_ppc_clients.remove(service_instance_id);
        let snapshot = state.service_snapshots.remove(service_instance_id);
        mark_service_stopped(&mut state, service_instance_id);
        drop(state);
        snapshot
    };
    if let Some(snapshot) = snapshot_to_release {
        release_service_snapshot(&snapshot);
    }
    Err(DaemonError::Plugin(PluginError::Ensure(format!(
        "service {service_instance_id} process exited before plugin dispatch"
    ))))
}

pub(super) fn teardown_failed_connected_service(
    service_instance_id: &str,
    reason: &str,
) -> Result<(), DaemonError> {
    let (process, snapshot) = {
        let mut state = lock_state()?;
        state.service_ppc_clients.remove(service_instance_id);
        let process = state.service_processes.remove(service_instance_id);
        let snapshot = state.service_snapshots.remove(service_instance_id);
        if let Ok(status) = service_status_mut(&mut state, service_instance_id) {
            status.state = PluginServiceState::Stopped;
            status.last_error = Some(reason.to_owned());
        }
        drop(state);
        (process, snapshot)
    };
    if let Some(mut process) = process {
        process.teardown();
    }
    if let Some(snapshot) = snapshot {
        release_service_snapshot(&snapshot);
    }
    Ok(())
}

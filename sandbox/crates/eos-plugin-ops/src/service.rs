use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eos_layerstack::{manifest_root_hash, LayerStack, Lease};
use eos_plugin::{PluginError, PluginServiceKey, PluginServiceState, PluginServiceStatus};

use crate::PluginRuntimeError;
use eos_ephemeral_workspace::overlay_run_dirs;

use super::process::PluginServiceOverlay;
use super::state::{DaemonPluginState, PluginRuntime, SharedPpcClient};
use crate::route::PluginProcessSpec;

#[derive(Debug, Clone)]
pub(super) struct PluginServiceSnapshot {
    pub(super) layer_stack_root: String,
    pub(super) lease_id: String,
    pub(super) manifest_key: String,
    pub(super) layer_paths: Vec<String>,
    pub(super) overlay: Option<PluginServiceOverlay>,
}

pub(super) struct StartedPluginService {
    service_instance_id: String,
    process: super::process::PluginServiceProcess,
    client: SharedPpcClient,
    snapshot: PluginServiceSnapshot,
}

pub(super) fn service_specs_to_start(
    state: &DaemonPluginState,
    specs: &[PluginProcessSpec],
) -> Vec<PluginProcessSpec> {
    specs
        .iter()
        .filter(|spec| {
            !state
                .service_processes
                .contains_key(&spec.service_instance_id())
        })
        .cloned()
        .collect()
}

impl PluginRuntime {
    pub(super) fn spawn_service_processes(
        &self,
        specs: &[PluginProcessSpec],
    ) -> Result<Vec<StartedPluginService>, PluginRuntimeError> {
        let mut started = Vec::with_capacity(specs.len());
        for spec in specs {
            let snapshot = acquire_service_snapshot(&spec.key, "start")?;
            let (process, client) = match super::process::spawn_connected_with_overlay(
                &*self.launcher,
                spec,
                snapshot.overlay.as_ref(),
                Duration::from_millis(self.config.service_probe_timeout_ms),
            ) {
                Ok(started) => started,
                Err(err) => {
                    release_service_snapshot(&snapshot);
                    return Err(err);
                }
            };
            started.push(StartedPluginService {
                service_instance_id: spec.service_instance_id(),
                process,
                client: Arc::new(client),
                snapshot,
            });
        }
        Ok(started)
    }
}

pub(super) fn insert_started_service_processes(
    state: &mut DaemonPluginState,
    started_services: Vec<StartedPluginService>,
) -> Result<usize, PluginRuntimeError> {
    let mut started_count = 0;
    for started in started_services {
        if state
            .service_processes
            .contains_key(&started.service_instance_id)
            || !service_process_still_declared(state, &started.service_instance_id)
        {
            release_service_snapshot(&started.snapshot);
            continue;
        }
        mark_service_ready(
            state,
            &started.service_instance_id,
            &started.snapshot,
            false,
        )?;
        state
            .service_ppc_clients
            .insert(started.service_instance_id.clone(), started.client);
        state
            .service_snapshots
            .insert(started.service_instance_id.clone(), started.snapshot);
        state
            .service_processes
            .insert(started.service_instance_id, started.process);
        started_count += 1;
    }
    Ok(started_count)
}

pub(super) fn acquire_service_snapshot(
    key: &PluginServiceKey,
    reason: &str,
) -> Result<PluginServiceSnapshot, PluginRuntimeError> {
    let stack = LayerStack::open(PathBuf::from(&key.layer_stack_root))?;
    let lease = stack.acquire_snapshot(&format!(
        "plugin-service:{}:{}:{reason}",
        key.plugin_id, key.service_id
    ))?;
    let mut snapshot = service_snapshot_from_lease(&key.layer_stack_root, lease);
    snapshot.overlay = service_overlay_for_snapshot(key, &snapshot)?;
    Ok(snapshot)
}

pub(super) fn active_manifest_key(layer_stack_root: &str) -> Result<String, PluginRuntimeError> {
    let manifest = LayerStack::open(PathBuf::from(layer_stack_root))?.read_active_manifest()?;
    Ok(manifest_key(
        manifest.version,
        &manifest_root_hash(&manifest),
    ))
}

pub(super) fn release_service_snapshot(snapshot: &PluginServiceSnapshot) {
    if let Some(overlay) = &snapshot.overlay {
        let _ = std::fs::remove_dir_all(&overlay.run_dir);
    }
    if let Ok(mut stack) = LayerStack::open(PathBuf::from(&snapshot.layer_stack_root)) {
        let _ = stack.release_lease(&snapshot.lease_id);
    }
}

pub(super) fn mark_service_ready(
    state: &mut DaemonPluginState,
    service_instance_id: &str,
    snapshot: &PluginServiceSnapshot,
    refreshed: bool,
) -> Result<(), PluginRuntimeError> {
    let status = service_status_mut(state, service_instance_id)?;
    status.state = PluginServiceState::Ready;
    status.manifest_key = Some(snapshot.manifest_key.clone());
    if refreshed {
        status.refresh_count = status.refresh_count.saturating_add(1);
    }
    status.last_error = None;
    Ok(())
}

pub(super) fn mark_service_restarted(
    state: &mut DaemonPluginState,
    service_instance_id: &str,
) -> Result<(), PluginRuntimeError> {
    let status = service_status_mut(state, service_instance_id)?;
    status.restart_count = status.restart_count.saturating_add(1);
    Ok(())
}

pub(super) fn mark_service_stale(
    state: &mut DaemonPluginState,
    service_instance_id: &str,
    reason: impl Into<String>,
) -> Result<(), PluginRuntimeError> {
    let status = service_status_mut(state, service_instance_id)?;
    status.state = PluginServiceState::Stale;
    status.last_error = Some(reason.into());
    Ok(())
}

pub(super) fn mark_service_stopped(state: &mut DaemonPluginState, service_instance_id: &str) {
    if let Ok(status) = service_status_mut(state, service_instance_id) {
        status.state = PluginServiceState::Stopped;
        status.last_error = Some("service process stopped".to_owned());
    }
}

pub(super) fn service_status_mut<'a>(
    state: &'a mut DaemonPluginState,
    service_instance_id: &str,
) -> Result<&'a mut PluginServiceStatus, PluginRuntimeError> {
    state
        .loaded
        .values_mut()
        .flat_map(|loaded| loaded.services.iter_mut())
        .find(|status| status.key.service_instance_id() == service_instance_id)
        .ok_or_else(|| {
            PluginRuntimeError::Plugin(PluginError::Ensure(format!(
                "service {service_instance_id} is not registered"
            )))
        })
}

pub(super) fn stop_plugin_service_processes(state: &mut DaemonPluginState, plugin_id: &str) {
    let stale_service_ids = state
        .loaded
        .get(plugin_id)
        .map(|loaded| {
            loaded
                .service_processes
                .iter()
                .map(PluginProcessSpec::service_instance_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    remove_service_instances(state, &stale_service_ids);
}

pub(super) fn stop_services_for_layer_stack_root(
    state: &mut DaemonPluginState,
    layer_stack_root: &str,
) -> usize {
    let service_instance_ids = state
        .service_snapshots
        .iter()
        .filter(|(_, snapshot)| snapshot.layer_stack_root == layer_stack_root)
        .map(|(service_instance_id, _)| service_instance_id.clone())
        .collect::<Vec<_>>();
    let stopped_count = service_instance_ids.len();
    remove_service_instances(state, &service_instance_ids);
    stopped_count
}

pub(super) fn running_process_statuses(
    state: &mut DaemonPluginState,
) -> Vec<super::process::ServiceProcessStatus> {
    let mut closed = Vec::new();
    let mut statuses = Vec::new();
    for (service_instance_id, process) in &mut state.service_processes {
        let status = process.status();
        if !status.running {
            closed.push(service_instance_id.clone());
        }
        statuses.push(status);
    }
    remove_service_instances(state, &closed);
    statuses
}

/// Reap service processes whose child has exited — the teardown half of
/// [`running_process_statuses`], for callers that only need the side effect.
pub(super) fn reap_exited_processes(state: &mut DaemonPluginState) {
    let mut closed = Vec::new();
    for (service_instance_id, process) in &mut state.service_processes {
        if !process.is_running() {
            closed.push(service_instance_id.clone());
        }
    }
    remove_service_instances(state, &closed);
}

/// Stop tracking each given service instance and release the snapshot lease it
/// held.
fn remove_service_instances(state: &mut DaemonPluginState, closed: &[String]) {
    for service_instance_id in closed {
        state.service_processes.remove(service_instance_id);
        state.service_ppc_clients.remove(service_instance_id);
        if let Some(snapshot) = state.service_snapshots.remove(service_instance_id) {
            release_service_snapshot(&snapshot);
        }
        mark_service_stopped(state, service_instance_id);
    }
}

fn service_snapshot_from_lease(layer_stack_root: &str, lease: Lease) -> PluginServiceSnapshot {
    PluginServiceSnapshot {
        layer_stack_root: layer_stack_root.to_owned(),
        lease_id: lease.lease_id,
        manifest_key: manifest_key(lease.manifest_version, &lease.root_hash),
        layer_paths: lease.layer_paths,
        overlay: None,
    }
}

fn service_overlay_for_snapshot(
    key: &PluginServiceKey,
    snapshot: &PluginServiceSnapshot,
) -> Result<Option<PluginServiceOverlay>, PluginRuntimeError> {
    let dirs = overlay_run_dirs(
        "plugin-service",
        &format!("{}-{}", key.service_id, snapshot.manifest_key),
    )
    .map_err(|err| PluginRuntimeError::OverlayPipeline(err.to_string()))?;
    Ok(Some(PluginServiceOverlay {
        run_dir: dirs.run_dir,
        layer_paths: snapshot.layer_paths.iter().map(PathBuf::from).collect(),
        upperdir: dirs.upperdir,
        workdir: dirs.workdir,
    }))
}

fn manifest_key(version: i64, root_hash: &str) -> String {
    format!("{version}:{root_hash}")
}

fn service_process_still_declared(state: &DaemonPluginState, service_instance_id: &str) -> bool {
    state.loaded.values().any(|loaded| {
        loaded
            .service_processes
            .iter()
            .any(|spec| spec.service_instance_id() == service_instance_id)
    })
}

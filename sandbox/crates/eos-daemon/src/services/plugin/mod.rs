//! Daemon plugin service runtime.
//!
//! This module owns the daemon-side `api.plugin.*` implementation behind the
//! `ops::plugin` adapter: service process lifetime, PPC dispatch, manifest
//! refresh, plugin-originated OCC callbacks, and oneshot overlay execution.
//! The boundary is DTO-in / outcome-out — wire `Value` parsing and response
//! shaping live in the adapter. All state lives on a [`PluginRuntime`]
//! instance owned by the server's `Services`, never in process globals.

mod callbacks;
mod connected;
mod dispatch;
mod overlay;
mod process;
mod refresh;
mod service;
mod setup;
mod state;

use std::time::Duration;

use eos_plugin::{PluginError, PluginManifest, PluginServiceStatus};
use eos_plugin_runtime::ensure::ParsedEnsure;
use eos_plugin_runtime::route::{PluginOperationRoute, PluginProcessSpec};
use eos_plugin_runtime::{ensure_package, PackageEnsureReport};
use serde_json::Value;

use crate::error::DaemonError;
use refresh::service_health_probe_targets;
use service::{
    insert_started_service_processes, reap_exited_processes, running_process_statuses,
    service_specs_to_start, stop_plugin_service_processes,
};
use state::loaded_matches_parsed;
use state::{connected_ppc_routes, connected_ppc_services, setup_failure_key};

pub(crate) use dispatch::PluginDispatchOutcome;
pub(crate) use overlay::PluginOverlayOutcome;
pub(crate) use process::ServiceProcessStatus;
pub(crate) use refresh::ServiceHealthReport;
pub(crate) use state::{PluginRuntime, SetupFailure};

/// Typed result of one `api.plugin.ensure` call.
pub(crate) enum EnsureOutcome {
    /// The package content for this digest is not published yet; the caller
    /// must upload before services can start.
    NeedsUpload {
        manifest: Box<PluginManifest>,
        report: PackageEnsureReport,
    },
    Ready(Box<EnsureReady>),
}

/// The registered (or re-confirmed) plugin runtime view after an ensure.
pub(crate) struct EnsureReady {
    pub(crate) plugin_id: String,
    pub(crate) digest: String,
    pub(crate) registered_ops: Vec<String>,
    pub(crate) runtime_loaded: bool,
    pub(crate) started_count: usize,
    pub(crate) already_loaded: bool,
    pub(crate) operation_routes: Vec<PluginOperationRoute>,
    pub(crate) services: Vec<PluginServiceStatus>,
    pub(crate) service_processes: Vec<PluginProcessSpec>,
    pub(crate) running_service_processes: Vec<ServiceProcessStatus>,
    pub(crate) connected_ppc_routes: Vec<String>,
    pub(crate) connected_ppc_services: Vec<String>,
    pub(crate) package: PackageEnsureReport,
}

/// One loaded plugin's registry view for `api.plugin.status`.
pub(crate) struct LoadedPluginStatus {
    pub(crate) name: String,
    pub(crate) digest: String,
    pub(crate) ops: Vec<String>,
    pub(crate) operation_routes: Vec<PluginOperationRoute>,
    pub(crate) services: Vec<PluginServiceStatus>,
    pub(crate) service_processes: Vec<PluginProcessSpec>,
    pub(crate) runtime_loaded: bool,
}

/// Typed result of one `api.plugin.status` call.
pub(crate) struct StatusOutcome {
    pub(crate) loaded_plugins: Vec<LoadedPluginStatus>,
    pub(crate) running_service_processes: Vec<ServiceProcessStatus>,
    pub(crate) connected_ppc_routes: Vec<String>,
    pub(crate) connected_ppc_services: Vec<String>,
    pub(crate) setup_failures: Vec<SetupFailure>,
    pub(crate) service_health: Vec<ServiceHealthReport>,
}

impl PluginRuntime {
    /// Register (or re-confirm) a plugin from its parsed ensure args, ensuring
    /// package content and optionally starting its declared service processes.
    pub(crate) fn ensure(
        &self,
        args: &Value,
        start_services: bool,
    ) -> Result<EnsureOutcome, DaemonError> {
        let parsed = ParsedEnsure::from_args(args, &self.ppc_socket_root())?;
        let package_report = match ensure_package(args, parsed.manifest.as_ref()) {
            Ok(report) => report,
            Err(err) => {
                let err = DaemonError::from(err);
                self.record_setup_failure(parsed.manifest.as_ref(), &err);
                return Err(err);
            }
        };
        if package_report.needs_upload {
            let manifest = parsed.manifest.ok_or_else(|| {
                DaemonError::Plugin(PluginError::Ensure(
                    "package ensure requested upload without manifest".to_owned(),
                ))
            })?;
            return Ok(EnsureOutcome::NeedsUpload {
                manifest: Box::new(manifest),
                report: package_report,
            });
        }
        let plugin_id = parsed.plugin_id.clone();
        let (already_loaded, specs_to_start) = {
            let mut state = self.lock_state()?;
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
                state.loaded.insert(parsed.plugin_id.clone(), parsed);
            }
            let process_specs = state
                .loaded
                .get(&plugin_id)
                .ok_or_else(|| ensure_not_recorded(&plugin_id))?
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
        let started_services = self.spawn_service_processes(&specs_to_start)?;
        let mut state = self.lock_state()?;
        let started_count = insert_started_service_processes(&mut state, started_services)?;
        let loaded = state
            .loaded
            .get(&plugin_id)
            .ok_or_else(|| ensure_not_recorded(&plugin_id))?;
        let ready = EnsureReady {
            plugin_id,
            digest: loaded.plugin_digest.clone(),
            registered_ops: loaded.registered_ops.clone(),
            runtime_loaded: loaded.runtime_loaded,
            started_count,
            already_loaded,
            operation_routes: loaded.operation_routes.values().cloned().collect(),
            services: loaded.services.clone(),
            service_processes: loaded.service_processes.clone(),
            running_service_processes: running_process_statuses(&mut state),
            connected_ppc_routes: connected_ppc_routes(&state),
            connected_ppc_services: connected_ppc_services(&state),
            package: package_report,
        };
        drop(state);
        Ok(EnsureOutcome::Ready(Box::new(ready)))
    }

    /// Report the loaded-plugin registry, live processes, and (optionally)
    /// connected-service health probes.
    pub(crate) fn status(
        &self,
        probe_services: bool,
        probe_timeout: Option<Duration>,
    ) -> Result<StatusOutcome, DaemonError> {
        let probe_timeout = probe_timeout
            .unwrap_or_else(|| Duration::from_millis(self.config.service_probe_timeout_ms));
        let probe_targets = {
            let mut state = self.lock_state()?;
            reap_exited_processes(&mut state);
            if probe_services {
                service_health_probe_targets(&state)
            } else {
                Vec::new()
            }
        };
        let service_health = self.probe_service_health(probe_targets, probe_timeout);
        let mut state = self.lock_state()?;
        let running_service_processes = running_process_statuses(&mut state);
        let loaded_plugins = state
            .loaded
            .iter()
            .map(|(name, loaded)| LoadedPluginStatus {
                name: name.clone(),
                digest: loaded.plugin_digest.clone(),
                ops: loaded.registered_ops.clone(),
                operation_routes: loaded.operation_routes.values().cloned().collect(),
                services: loaded.services.clone(),
                service_processes: loaded.service_processes.clone(),
                runtime_loaded: loaded.runtime_loaded,
            })
            .collect();
        let outcome = StatusOutcome {
            loaded_plugins,
            running_service_processes,
            connected_ppc_routes: connected_ppc_routes(&state),
            connected_ppc_services: connected_ppc_services(&state),
            setup_failures: state.setup_failures.values().cloned().collect(),
            service_health,
        };
        drop(state);
        Ok(outcome)
    }

    #[cfg(test)]
    fn register_ppc_client_for_tests(
        &self,
        op: &str,
        stream: std::os::unix::net::UnixStream,
    ) -> Result<(), DaemonError> {
        use eos_plugin::PluginServiceState;
        use service::{active_manifest_key, service_status_mut};
        use std::sync::Arc;

        let mut state = self.lock_state()?;
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
            Arc::new(eos_plugin_runtime::PpcClient::new(stream)?),
        );
        drop(state);
        Ok(())
    }
}

fn ensure_not_recorded(plugin_id: &str) -> DaemonError {
    DaemonError::Plugin(PluginError::Ensure(format!(
        "plugin {plugin_id} was not recorded after ensure"
    )))
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/mod.rs"]
mod tests;

//! Host-neutral plugin operation runtime.
//!
//! This crate owns service process lifetime, PPC transport and dispatch,
//! manifest refresh, package publish/setup, `api.plugin.ensure` parsing, and
//! oneshot overlay execution. Wire parsing and response shaping stay in the
//! daemon's `ops/plugin` adapter.
//!
//! The OCC single writer stays **daemon-shared**: plugin-originated commit
//! callbacks and oneshot overlay publishes route through
//! `eos_layerstack::service`, the same per-root writer as the primary write
//! paths. The daemon folds [`PluginRuntimeError`] / [`PpcError`] onto its own
//! error algebra.

mod callbacks;
mod dispatch;
pub mod ensure;
mod overlay;
pub(crate) mod package;
mod process;
mod refresh;
pub mod route;
mod service;
mod state;
pub(crate) mod transport;

use std::time::Duration;

use eos_plugin::{PluginError, PluginManifest, PluginServiceStatus};
use serde_json::Value;

use self::ensure::ParsedEnsure;
use self::refresh::service_health_probe_targets;
use self::route::{PluginOperationRoute, PluginProcessSpec};
use self::service::{
    insert_started_service_processes, reap_exited_processes, running_process_statuses,
    service_specs_to_start, stop_plugin_service_processes,
    stop_services_for_layer_stack_root as stop_services_for_layer_stack_root_in_state,
};
use self::state::loaded_matches_parsed;
use self::state::{connected_ppc_routes, connected_ppc_services, setup_failure_key};

pub use self::dispatch::PluginDispatchOutcome;
pub use self::overlay::PluginOverlayOutcome;
pub use self::package::{needs_upload_response, PackageEnsureReport};
pub use self::process::ServiceProcessStatus;
pub use self::refresh::ServiceHealthReport;
pub use self::state::{PluginRuntime, SetupFailure};
pub use self::transport::{read_message_bytes, PpcClient};
pub use eos_isolated_workspace::{LaunchError, NsRunnerLauncher};

/// Failures surfaced by the plugin PPC transport and package pipeline.
///
/// This is the local error the transport/package code raises in place of the
/// daemon's `DaemonError`; the daemon re-maps each variant back onto its own
/// error (preserving the inner [`eos_plugin::PluginError`], so the dispatcher
/// still classifies `ForbiddenInIsolatedWorkspace` correctly).
#[derive(Debug, thiserror::Error)]
pub enum PpcError {
    /// A typed plugin-contract failure (PPC wire messages, ensure, manifest, …).
    #[error(transparent)]
    Plugin(#[from] eos_plugin::PluginError),

    /// A PPC message could not be encoded / parsed.
    #[error(transparent)]
    Protocol(#[from] eos_plugin::wire::WireError),

    /// A socket / filesystem I/O operation failed.
    #[error("plugin ppc io error: {0}")]
    Io(#[from] std::io::Error),

    /// A process-local PPC state mutex was poisoned.
    #[error("daemon state lock poisoned: {0}")]
    LockPoisoned(&'static str),

    /// An injected callback handler failed; carries the handler's message text
    /// verbatim so the daemon's re-map reproduces the original error string.
    #[error("{0}")]
    Callback(String),
}

/// Failures surfaced by the plugin service runtime. The daemon folds each
/// variant onto its matching `DaemonError` variant, preserving message text so
/// wire responses do not drift.
#[derive(Debug, thiserror::Error)]
pub enum PluginRuntimeError {
    /// A typed plugin-contract failure.
    #[error(transparent)]
    Plugin(#[from] PluginError),

    /// A PPC transport / package pipeline failure.
    #[error(transparent)]
    Ppc(#[from] PpcError),

    /// An ns-runner launch failure.
    #[error(transparent)]
    Launch(#[from] LaunchError),

    /// The runtime's state mutex was poisoned.
    #[error("daemon state lock poisoned: {0}")]
    StateLockPoisoned(&'static str),

    /// The overlay run-dir / capture pipeline failed.
    #[error("{0}")]
    OverlayPipeline(String),

    /// A structurally invalid request reached the runtime.
    #[error("{0}")]
    InvalidRequest(String),

    /// A filesystem / socket I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The layer-stack storage / lease layer failed.
    #[error(transparent)]
    LayerStack(#[from] eos_layerstack::LayerStackError),

    /// The OCC publish path failed.
    #[error(transparent)]
    Commit(#[from] eos_layerstack::CommitError),
}

/// Typed result of one `api.plugin.ensure` call.
pub enum EnsureOutcome {
    /// The package content for this digest is not published yet; the caller
    /// must upload before services can start.
    NeedsUpload {
        /// The parsed plugin manifest the upload belongs to.
        manifest: Box<PluginManifest>,
        /// The package report describing the missing upload.
        report: PackageEnsureReport,
    },
    /// The plugin is registered and its package content is in place.
    Ready(Box<EnsureReady>),
}

/// The registered (or re-confirmed) plugin runtime view after an ensure.
pub struct EnsureReady {
    pub plugin_id: String,
    pub digest: String,
    pub registered_ops: Vec<String>,
    pub runtime_loaded: bool,
    pub started_count: usize,
    pub already_loaded: bool,
    pub operation_routes: Vec<PluginOperationRoute>,
    pub services: Vec<PluginServiceStatus>,
    pub service_processes: Vec<PluginProcessSpec>,
    pub running_service_processes: Vec<ServiceProcessStatus>,
    pub connected_ppc_routes: Vec<String>,
    pub connected_ppc_services: Vec<String>,
    pub package: PackageEnsureReport,
}

/// One loaded plugin's registry view for `api.plugin.status`.
pub struct LoadedPluginStatus {
    pub name: String,
    pub digest: String,
    pub ops: Vec<String>,
    pub operation_routes: Vec<PluginOperationRoute>,
    pub services: Vec<PluginServiceStatus>,
    pub service_processes: Vec<PluginProcessSpec>,
    pub runtime_loaded: bool,
}

/// Typed result of one `api.plugin.status` call.
pub struct StatusOutcome {
    pub loaded_plugins: Vec<LoadedPluginStatus>,
    pub running_service_processes: Vec<ServiceProcessStatus>,
    pub connected_ppc_routes: Vec<String>,
    pub connected_ppc_services: Vec<String>,
    pub setup_failures: Vec<SetupFailure>,
    pub service_health: Vec<ServiceHealthReport>,
}

impl PluginRuntime {
    fn ppc_socket_root(&self) -> String {
        self.config.ppc_root.to_string_lossy().into_owned()
    }

    fn record_setup_failure(&self, manifest: Option<&PluginManifest>, err: &PluginRuntimeError) {
        let Some(manifest) = manifest else {
            return;
        };
        if let Ok(mut state) = self.lock_state() {
            state.setup_failures.insert(
                setup_failure_key(&manifest.plugin_id, &manifest.plugin_digest),
                SetupFailure {
                    plugin: manifest.plugin_id.clone(),
                    digest: manifest.plugin_digest.clone(),
                    error: err.to_string(),
                },
            );
        }
    }

    /// Stop and forget every connected service holding a snapshot on
    /// `layer_stack_root` (the workspace-base reset path).
    pub fn stop_services_for_layer_stack_root(
        &self,
        layer_stack_root: &str,
    ) -> Result<usize, PluginRuntimeError> {
        let mut state = self.lock_state()?;
        Ok(stop_services_for_layer_stack_root_in_state(
            &mut state,
            layer_stack_root,
        ))
    }

    /// Register (or re-confirm) a plugin from its parsed ensure args, ensuring
    /// package content and optionally starting its declared service processes.
    ///
    /// # Errors
    ///
    /// Returns a [`PluginRuntimeError`] when ensure parsing, the package
    /// pipeline, or service process startup fails.
    pub fn ensure(
        &self,
        args: &Value,
        start_services: bool,
    ) -> Result<EnsureOutcome, PluginRuntimeError> {
        let parsed = ParsedEnsure::from_args(args, &self.ppc_socket_root())?;
        let package_report = match package::ensure_package(args, parsed.manifest.as_ref()) {
            Ok(report) => report,
            Err(err) => {
                let err = PluginRuntimeError::from(err);
                self.record_setup_failure(parsed.manifest.as_ref(), &err);
                return Err(err);
            }
        };
        if package_report.needs_upload {
            let manifest = parsed.manifest.ok_or_else(|| {
                PluginRuntimeError::Plugin(PluginError::Ensure(
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
    /// connected-service health probes. `probe_timeout` defaults to the
    /// configured service probe timeout.
    ///
    /// # Errors
    ///
    /// Returns a [`PluginRuntimeError`] when the runtime state lock is
    /// poisoned.
    pub fn status(
        &self,
        probe_services: bool,
        probe_timeout: Option<Duration>,
    ) -> Result<StatusOutcome, PluginRuntimeError> {
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
}

fn ensure_not_recorded(plugin_id: &str) -> PluginRuntimeError {
    PluginRuntimeError::Plugin(PluginError::Ensure(format!(
        "plugin {plugin_id} was not recorded after ensure"
    )))
}

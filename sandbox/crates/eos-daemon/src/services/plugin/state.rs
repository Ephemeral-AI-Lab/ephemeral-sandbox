use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use eos_config::configs::daemon::PluginRuntimeConfig;
use eos_plugin::PluginServiceStatus;
use eos_plugin_runtime::ensure::ParsedEnsure;
use serde::Serialize;

use super::{process::PluginServiceProcess, service::PluginServiceSnapshot};
use crate::error::DaemonError;

pub(super) type SharedPpcClient = Arc<eos_plugin_runtime::PpcClient>;

/// One recorded plugin setup failure (the wire view is produced by
/// serialization at the adapter).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupFailure {
    pub(super) plugin: String,
    pub(super) digest: String,
    pub(super) error: String,
}

#[derive(Default)]
pub(super) struct DaemonPluginState {
    /// Live plugin registrations, keyed by plugin id; the stored
    /// [`ParsedEnsure`] is the spec of record and its `services` statuses are
    /// mutated in place as processes start/stop/refresh.
    pub(super) loaded: BTreeMap<String, ParsedEnsure>,
    pub(super) service_ppc_clients: BTreeMap<String, SharedPpcClient>,
    pub(super) service_processes: BTreeMap<String, PluginServiceProcess>,
    pub(super) service_snapshots: BTreeMap<String, PluginServiceSnapshot>,
    pub(super) service_refresh_locks: BTreeMap<String, Arc<Mutex<()>>>,
    pub(super) setup_failures: BTreeMap<String, SetupFailure>,
}

/// Instance-owned plugin service runtime: the typed config plus the registry
/// of loaded plugins, service processes, PPC clients, and snapshots.
pub(crate) struct PluginRuntime {
    pub(super) config: PluginRuntimeConfig,
    state: Mutex<DaemonPluginState>,
}

impl PluginRuntime {
    pub(crate) fn new(config: PluginRuntimeConfig) -> Self {
        Self {
            config,
            state: Mutex::new(DaemonPluginState::default()),
        }
    }

    pub(super) fn lock_state(&self) -> Result<MutexGuard<'_, DaemonPluginState>, DaemonError> {
        self.state
            .lock()
            .map_err(|_| DaemonError::StateLockPoisoned("plugin registry"))
    }
}

pub(super) fn connected_ppc_routes(state: &DaemonPluginState) -> Vec<String> {
    state
        .loaded
        .values()
        .flat_map(|loaded| loaded.operation_routes.values())
        .filter(|route| {
            route
                .service_instance_id
                .as_ref()
                .is_some_and(|service_instance_id| {
                    state.service_ppc_clients.contains_key(service_instance_id)
                })
        })
        .map(|route| route.public_op.clone())
        .collect()
}

pub(super) fn connected_ppc_services(state: &DaemonPluginState) -> Vec<String> {
    state.service_ppc_clients.keys().cloned().collect()
}

pub(super) fn find_service_status<'a>(
    state: &'a DaemonPluginState,
    service_instance_id: &str,
) -> Option<&'a PluginServiceStatus> {
    state
        .loaded
        .values()
        .flat_map(|loaded| loaded.services.iter())
        .find(|status| status.key.service_instance_id() == service_instance_id)
}

pub(super) fn setup_failure_key(plugin_id: &str, plugin_digest: &str) -> String {
    format!("{plugin_id}:{plugin_digest}")
}

/// Whether the live registration already matches a freshly parsed ensure —
/// lets `ensure` skip re-registering. Compares only the immutable spec fields;
/// the stored `services` statuses mutate at runtime and never participate.
pub(super) fn loaded_matches_parsed(loaded: &ParsedEnsure, parsed: &ParsedEnsure) -> bool {
    loaded.plugin_digest == parsed.plugin_digest
        && loaded.registered_ops == parsed.registered_ops
        && loaded.operation_routes == parsed.operation_routes
        && loaded.service_processes == parsed.service_processes
        && loaded.runtime_loaded == parsed.runtime_loaded
}

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use eos_plugin::host::ensure_args::ParsedEnsure;
use eos_plugin::host::route::{PluginOperationRoute, PluginProcessSpec};
use eos_plugin::PluginServiceStatus;
use serde_json::{json, Value};

use super::{
    process::{process_spec_to_json, PluginServiceProcess},
    service::PluginServiceSnapshot,
};
use crate::error::DaemonError;

pub(super) type SharedPpcClient = Arc<eos_plugin::host::PpcClient>;

#[derive(Debug, Clone)]
pub(super) struct LoadedPluginRuntime {
    pub(super) digest: String,
    pub(super) registered_ops: Vec<String>,
    pub(super) operation_routes: BTreeMap<String, PluginOperationRoute>,
    pub(super) services: Vec<PluginServiceStatus>,
    pub(super) service_processes: Vec<PluginProcessSpec>,
    pub(super) runtime_loaded: bool,
}

/// Wire-shape one resolved route. Daemon-owned (the route data lives host-side).
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

#[derive(Debug, Default)]
pub(super) struct DaemonPluginState {
    pub(super) loaded: BTreeMap<String, LoadedPluginRuntime>,
    pub(super) service_ppc_clients: BTreeMap<String, SharedPpcClient>,
    pub(super) service_processes: BTreeMap<String, PluginServiceProcess>,
    pub(super) service_snapshots: BTreeMap<String, PluginServiceSnapshot>,
    pub(super) service_refresh_locks: BTreeMap<String, Arc<Mutex<()>>>,
    pub(super) setup_failures: BTreeMap<String, Value>,
}

fn state_cell() -> &'static Mutex<DaemonPluginState> {
    static STATE: OnceLock<Mutex<DaemonPluginState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(DaemonPluginState::default()))
}

pub(super) fn lock_state() -> Result<MutexGuard<'static, DaemonPluginState>, DaemonError> {
    state_cell()
        .lock()
        .map_err(|_| DaemonError::StateLockPoisoned("plugin registry"))
}

#[cfg(test)]
pub(super) fn reset_state_for_tests() -> Vec<PluginServiceSnapshot> {
    let Ok(mut state) = state_cell().lock() else {
        return Vec::new();
    };
    let snapshots = state
        .service_snapshots
        .values()
        .cloned()
        .collect::<Vec<_>>();
    state.loaded.clear();
    state.service_ppc_clients.clear();
    state.service_processes.clear();
    state.service_snapshots.clear();
    state.service_refresh_locks.clear();
    state.setup_failures.clear();
    snapshots
}

pub(super) fn route_values(routes: &BTreeMap<String, PluginOperationRoute>) -> Vec<Value> {
    routes.values().map(route_to_json).collect()
}

pub(super) fn process_values(processes: &[PluginProcessSpec]) -> Vec<Value> {
    processes.iter().map(process_spec_to_json).collect()
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

pub(super) fn setup_failure_values(state: &DaemonPluginState) -> Vec<Value> {
    state.setup_failures.values().cloned().collect()
}

pub(super) fn loaded_plugin_values(state: &DaemonPluginState) -> Vec<Value> {
    state
        .loaded
        .iter()
        .map(|(name, loaded)| {
            json!({
                "name": name,
                "digest": loaded.digest,
                "ops": loaded.registered_ops,
                "operation_routes": route_values(&loaded.operation_routes),
                "services": loaded.services,
                "service_processes": process_values(&loaded.service_processes),
                "runtime_loaded": loaded.runtime_loaded,
            })
        })
        .collect()
}

/// Whether the live runtime already matches a freshly parsed ensure — lets
/// `op_ensure` skip re-registering. Compares the daemon's `LoadedPluginRuntime`
/// against the host-parsed [`ParsedEnsure`].
pub(super) fn loaded_matches_parsed(loaded: &LoadedPluginRuntime, parsed: &ParsedEnsure) -> bool {
    loaded.digest == parsed.plugin_digest
        && loaded.registered_ops == parsed.registered_ops
        && loaded.operation_routes == parsed.operation_routes
        && loaded.service_processes == parsed.service_processes
        && loaded.runtime_loaded == parsed.runtime_loaded
}

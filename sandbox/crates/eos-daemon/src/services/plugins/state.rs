use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use eos_plugin::{PluginServiceKey, PluginServiceStatus, ServiceMode};
use eos_protocol::Intent;
use serde_json::{json, Value};

use super::{
    ppc_router,
    process::{PluginProcessSpec, PluginServiceProcess},
    service::PluginServiceSnapshot,
};
use crate::error::DaemonError;

pub(super) type SharedPpcClient = Arc<ppc_router::PpcClient>;
pub(super) const MAX_PLUGIN_CALLER_FIELD_CHARS: usize = 256;

#[derive(Debug, Clone)]
pub(super) struct LoadedPluginRuntime {
    pub(super) digest: String,
    pub(super) registered_ops: Vec<String>,
    pub(super) operation_routes: BTreeMap<String, PluginOperationRoute>,
    pub(super) services: Vec<PluginServiceStatus>,
    pub(super) service_processes: Vec<PluginProcessSpec>,
    pub(super) runtime_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PluginOperationRoute {
    pub(super) plugin_id: String,
    pub(super) op_name: String,
    pub(super) public_op: String,
    pub(super) layer_stack_root: Option<String>,
    pub(super) intent: Intent,
    pub(super) auto_workspace_overlay: bool,
    pub(super) service_id: Option<String>,
    pub(super) service_instance_id: Option<String>,
    pub(super) service_key: Option<PluginServiceKey>,
    pub(super) service_mode: Option<ServiceMode>,
    pub(super) service_command: Vec<String>,
    pub(super) service_ppc_protocol_version: Option<u32>,
    pub(super) timeout_ms: Option<u64>,
}

impl PluginOperationRoute {
    pub(super) const fn dispatch_mode(&self) -> &'static str {
        match self.intent {
            Intent::ReadOnly => "read_only_service",
            Intent::WriteAllowed if self.auto_workspace_overlay => "write_allowed_oneshot_overlay",
            Intent::WriteAllowed => "self_managed_callback",
            Intent::Lifecycle => "invalid_lifecycle",
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "plugin": self.plugin_id,
            "op_name": self.op_name,
            "public_op": self.public_op,
            "layer_stack_root": self.layer_stack_root,
            "intent": self.intent,
            "auto_workspace_overlay": self.auto_workspace_overlay,
            "service_id": self.service_id,
            "service_instance_id": self.service_instance_id,
            "service_mode": self.service_mode,
            "service_command": self.service_command,
            "timeout_ms": self.timeout_ms,
            "dispatch_mode": self.dispatch_mode(),
        })
    }
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
    routes.values().map(PluginOperationRoute::to_json).collect()
}

pub(super) fn process_values(processes: &[PluginProcessSpec]) -> Vec<Value> {
    processes.iter().map(PluginProcessSpec::to_json).collect()
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

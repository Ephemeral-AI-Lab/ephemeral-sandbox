//! Daemon plugin API surface.
//!
//! This module owns the daemon-side `api.plugin.*` routes. It keeps the
//! contract-only `eos-plugin` crate free of sandbox publish edges while the
//! daemon owns service process lifetime, PPC dispatch, manifest refresh,
//! plugin-originated OCC callbacks, and oneshot overlay execution.

mod occ_callbacks;
mod ppc_router;
mod process;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use eos_layerstack::{manifest_root_hash, LayerStack, Lease};
#[cfg(all(target_os = "linux", not(test)))]
use eos_overlay::{allocate_overlay_writable_dirs, overlay_writable_root};
use eos_plugin::{
    public_op_name, PluginError, PluginManifest, PluginServiceKey, PluginServiceKeyParts,
    PluginServiceManifest, PluginServiceState, PluginServiceStatus, PpcDirection, PpcEnvelope,
    RefreshAck, RefreshRequest, RefreshStrategy, ServiceMode,
};
use eos_protocol::Intent;
use serde_json::{json, Value};

use crate::dispatcher::{DispatchContext, PluginOverlayCommand};
use crate::error::DaemonError;
use process::{PluginProcessSpec, PluginServiceOverlay};

type SharedPpcClient = Arc<ppc_router::PpcClient>;

const WORKSPACE_SNAPSHOT_REFRESH_OP: &str = "daemon.workspace_snapshot_refresh";

#[derive(Debug, Clone)]
struct LoadedPluginRuntime {
    digest: String,
    registered_ops: Vec<String>,
    operation_routes: BTreeMap<String, PluginOperationRoute>,
    services: Vec<PluginServiceStatus>,
    service_processes: Vec<PluginProcessSpec>,
    runtime_loaded: bool,
}

#[derive(Debug, Clone)]
struct PluginServiceSnapshot {
    layer_stack_root: String,
    lease_id: String,
    manifest_key: String,
    layer_paths: Vec<String>,
    overlay: Option<PluginServiceOverlay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginOperationRoute {
    plugin_id: String,
    op_name: String,
    public_op: String,
    layer_stack_root: Option<String>,
    intent: Intent,
    auto_workspace_overlay: bool,
    service_id: Option<String>,
    service_instance_id: Option<String>,
    service_key: Option<PluginServiceKey>,
    service_mode: Option<ServiceMode>,
    service_command: Vec<String>,
    service_ppc_protocol_version: Option<u32>,
    timeout_ms: Option<u64>,
}

impl PluginOperationRoute {
    const fn dispatch_mode(&self) -> &'static str {
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
struct DaemonPluginState {
    loaded: BTreeMap<String, LoadedPluginRuntime>,
    service_ppc_clients: BTreeMap<String, SharedPpcClient>,
    service_processes: BTreeMap<String, process::PluginServiceProcess>,
    service_snapshots: BTreeMap<String, PluginServiceSnapshot>,
    service_refresh_locks: BTreeMap<String, Arc<Mutex<()>>>,
}

fn state_cell() -> &'static Mutex<DaemonPluginState> {
    static STATE: OnceLock<Mutex<DaemonPluginState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(DaemonPluginState::default()))
}

fn lock_state() -> Result<MutexGuard<'static, DaemonPluginState>, DaemonError> {
    state_cell()
        .lock()
        .map_err(|_| DaemonError::StateLockPoisoned("plugin registry"))
}

pub fn op_ensure(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    ensure_plugin_family_allowed(args)?;

    let parsed = ParsedEnsure::from_args(args)?;
    let start_services = args
        .get("start_services")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (already_loaded, specs_to_start) = {
        let mut state = lock_state()?;
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
    }))
}

pub fn op_status(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    ensure_plugin_family_allowed(args)?;
    let probe_services = args
        .get("probe_services")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let probe_timeout = Duration::from_millis(
        args.get("probe_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(ppc_router::DEFAULT_PLUGIN_PPC_TIMEOUT_MS),
    );
    let probe_targets = {
        let mut state = lock_state()?;
        let _ = running_process_values(&mut state);
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
    drop(state);
    Ok(json!({
        "success": true,
        "loaded_plugins": loaded_plugins,
        "running_service_processes": running_service_processes,
        "connected_ppc_routes": connected_ppc_routes,
        "connected_ppc_services": connected_ppc_services,
        "service_health": service_health,
        "pending": [],
    }))
}

pub fn dispatch_registered_op(
    op: &str,
    invocation_id: &str,
    args: &Value,
    _context: DispatchContext<'_>,
) -> Option<Result<Value, DaemonError>> {
    if !op.starts_with("plugin.") {
        return None;
    }
    if let Err(err) = ensure_plugin_family_allowed(args) {
        return Some(Err(err));
    }
    let route = match route_for_op(op) {
        Ok(Some(route)) => route,
        Ok(None) => return None,
        Err(err) => return Some(Err(err)),
    };
    Some(dispatch_registered_route(&route, invocation_id, args))
}

#[cfg(test)]
fn reset_for_tests() {
    if let Ok(mut state) = state_cell().lock() {
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
        drop(state);
        for snapshot in snapshots {
            release_service_snapshot(&snapshot);
        }
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
        Arc::new(ppc_router::PpcClient::new(stream)?),
    );
    drop(state);
    Ok(())
}

fn ensure_plugin_family_allowed(args: &Value) -> Result<(), DaemonError> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !agent_id.is_empty() && crate::isolated::agent_has_active_handle(agent_id) {
        return Err(DaemonError::Plugin(
            PluginError::ForbiddenInIsolatedWorkspace,
        ));
    }
    Ok(())
}

struct ParsedEnsure {
    plugin_id: String,
    plugin_digest: String,
    registered_ops: Vec<String>,
    operation_routes: BTreeMap<String, PluginOperationRoute>,
    services: Vec<PluginServiceStatus>,
    service_processes: Vec<PluginProcessSpec>,
    runtime_loaded: bool,
}

fn loaded_matches_parsed(loaded: &LoadedPluginRuntime, parsed: &ParsedEnsure) -> bool {
    loaded.digest == parsed.plugin_digest
        && loaded.registered_ops == parsed.registered_ops
        && loaded.operation_routes == parsed.operation_routes
        && loaded.service_processes == parsed.service_processes
        && loaded.runtime_loaded == parsed.runtime_loaded
}

impl ParsedEnsure {
    fn from_args(args: &Value) -> Result<Self, DaemonError> {
        if let Some(manifest_value) = args.get("manifest") {
            let manifest: PluginManifest = serde_json::from_value(manifest_value.clone())
                .map_err(|err| PluginError::Manifest(err.to_string()))?;
            manifest.validate()?;
            return Self::from_manifest(args, manifest);
        }

        let plugin_id = args
            .get("plugin")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        validate_public_identifier("plugin", &plugin_id)?;
        let plugin_digest = args
            .get("digest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        Ok(Self {
            plugin_id,
            plugin_digest,
            registered_ops: Vec::new(),
            operation_routes: BTreeMap::new(),
            services: Vec::new(),
            service_processes: Vec::new(),
            runtime_loaded: false,
        })
    }

    fn from_manifest(args: &Value, manifest: PluginManifest) -> Result<Self, DaemonError> {
        let ppc_socket_root = ppc_socket_root(args);
        let layer_stack_root = args
            .get("layer_stack_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|root| !root.is_empty())
            .map(str::to_owned);
        let service_keys = service_keys_for_manifest(args, &manifest)?;
        let operation_routes =
            operation_routes_for_manifest(&manifest, &service_keys, layer_stack_root.as_deref());
        let registered_ops = operation_routes.keys().cloned().collect::<Vec<_>>();
        let (services, service_processes) =
            services_for_manifest(&manifest, &service_keys, &registered_ops, &ppc_socket_root)?;
        Ok(Self {
            plugin_id: manifest.plugin_id,
            plugin_digest: manifest.plugin_digest,
            registered_ops,
            operation_routes,
            services,
            service_processes,
            runtime_loaded: true,
        })
    }
}

fn operation_routes_for_manifest(
    manifest: &PluginManifest,
    service_keys: &BTreeMap<String, PluginServiceKey>,
    layer_stack_root: Option<&str>,
) -> BTreeMap<String, PluginOperationRoute> {
    manifest
        .operations
        .iter()
        .map(|op| {
            let public_op = public_op_name(&manifest.plugin_id, &op.op_name);
            let service = op.service_id.as_ref().and_then(|service_id| {
                manifest
                    .services
                    .iter()
                    .find(|service| service.service_id == *service_id)
            });
            let service_key = op
                .service_id
                .as_ref()
                .and_then(|service_id| service_keys.get(service_id))
                .cloned();
            (
                public_op.clone(),
                PluginOperationRoute {
                    plugin_id: manifest.plugin_id.clone(),
                    op_name: op.op_name.clone(),
                    public_op,
                    layer_stack_root: layer_stack_root.map(str::to_owned),
                    intent: op.intent,
                    auto_workspace_overlay: op.auto_workspace_overlay,
                    service_id: op.service_id.clone(),
                    service_instance_id: service_key
                        .as_ref()
                        .map(PluginServiceKey::service_instance_id),
                    service_key,
                    service_mode: service.map(|service| service.service_mode),
                    service_command: service
                        .map(|service| service.command.clone())
                        .unwrap_or_default(),
                    service_ppc_protocol_version: service
                        .map(|service| service.ppc_protocol_version),
                    timeout_ms: op.timeout_ms,
                },
            )
        })
        .collect()
}

fn services_for_manifest(
    manifest: &PluginManifest,
    service_keys: &BTreeMap<String, PluginServiceKey>,
    registered_ops: &[String],
    ppc_socket_root: &str,
) -> Result<(Vec<PluginServiceStatus>, Vec<PluginProcessSpec>), PluginError> {
    if manifest.services.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut process_specs = Vec::new();
    let statuses = manifest
        .services
        .iter()
        .map(|service| {
            let key = service_keys
                .get(&service.service_id)
                .ok_or_else(|| {
                    PluginError::Manifest(format!(
                        "service {} key was not prepared",
                        service.service_id
                    ))
                })?
                .clone();
            let mut status = PluginServiceStatus::new(key.clone());
            status.state = PluginServiceState::Stopped;
            status.registered_ops.clone_from(&registered_ops.to_vec());
            status.last_error = Some(service_initial_status_message(service.service_mode));
            if service.service_mode == ServiceMode::WorkspaceSnapshotRefresh
                && !service.command.is_empty()
            {
                process_specs.push(process_spec(&key, service, ppc_socket_root)?);
            }
            Ok(status)
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    Ok((statuses, process_specs))
}

fn service_initial_status_message(service_mode: ServiceMode) -> String {
    match service_mode {
        ServiceMode::WorkspaceSnapshotRefresh => {
            "process-backed PPC execution is not started".to_owned()
        }
        ServiceMode::OneshotOverlay => "oneshot overlay worker starts per operation".to_owned(),
        _ => "unsupported plugin service mode".to_owned(),
    }
}

fn process_spec(
    key: &PluginServiceKey,
    service: &PluginServiceManifest,
    ppc_socket_root: &str,
) -> Result<PluginProcessSpec, PluginError> {
    if ppc_socket_root == process::PLUGIN_PPC_ROOT {
        return PluginProcessSpec::new(
            key.clone(),
            service.command.clone(),
            service.ppc_protocol_version,
        );
    }
    PluginProcessSpec::new_with_socket_root(
        key.clone(),
        service.command.clone(),
        service.ppc_protocol_version,
        ppc_socket_root,
    )
}

fn service_keys_for_manifest(
    args: &Value,
    manifest: &PluginManifest,
) -> Result<BTreeMap<String, PluginServiceKey>, DaemonError> {
    if manifest.services.is_empty() {
        return Ok(BTreeMap::new());
    }
    let layer_stack_root = require_string(args, "layer_stack_root")?;
    let workspace_root = require_string(args, "workspace_root")?;
    manifest
        .services
        .iter()
        .map(|service| {
            let key = PluginServiceKey::new(PluginServiceKeyParts {
                layer_stack_root: layer_stack_root.clone(),
                workspace_root: workspace_root.clone(),
                plugin_id: manifest.plugin_id.clone(),
                plugin_digest: manifest.plugin_digest.clone(),
                service_id: service.service_id.clone(),
                service_profile_digest: service.service_profile_digest.clone(),
                service_mode: service.service_mode,
                refresh_strategy: service.refresh_strategy,
            })?;
            Ok((service.service_id.clone(), key))
        })
        .collect::<Result<BTreeMap<_, _>, PluginError>>()
        .map_err(DaemonError::from)
}

fn ppc_socket_root(args: &Value) -> String {
    #[cfg(test)]
    {
        if let Some(root) = args.get("ppc_socket_root").and_then(Value::as_str) {
            return root.to_owned();
        }
    }
    let _ = args;
    process::PLUGIN_PPC_ROOT.to_owned()
}

fn require_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(DaemonError::Plugin(PluginError::Ensure(format!(
            "api.plugin.ensure requires {key}"
        ))));
    }
    Ok(value)
}

fn validate_public_identifier(field: &str, value: &str) -> Result<(), DaemonError> {
    if value.is_empty() {
        return Err(DaemonError::Plugin(PluginError::Ensure(format!(
            "api.plugin.ensure requires {field} name"
        ))));
    }
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => {
            return Err(DaemonError::Plugin(PluginError::Ensure(format!(
                "{field} must start with an ASCII letter or underscore"
            ))));
        }
    }
    if chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(DaemonError::Plugin(PluginError::Ensure(format!(
            "{field} contains unsupported characters"
        ))))
    }
}

fn route_values(routes: &BTreeMap<String, PluginOperationRoute>) -> Vec<Value> {
    routes.values().map(PluginOperationRoute::to_json).collect()
}

fn process_values(processes: &[PluginProcessSpec]) -> Vec<Value> {
    processes.iter().map(PluginProcessSpec::to_json).collect()
}

fn connected_ppc_routes(state: &DaemonPluginState) -> Vec<String> {
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

fn connected_ppc_services(state: &DaemonPluginState) -> Vec<String> {
    state.service_ppc_clients.keys().cloned().collect()
}

struct StartedPluginService {
    service_instance_id: String,
    process: process::PluginServiceProcess,
    client: SharedPpcClient,
    snapshot: PluginServiceSnapshot,
}

fn service_specs_to_start(
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

fn spawn_service_processes(
    specs: &[PluginProcessSpec],
) -> Result<Vec<StartedPluginService>, DaemonError> {
    let mut started = Vec::with_capacity(specs.len());
    for spec in specs {
        let snapshot = acquire_service_snapshot(spec.key(), "start")?;
        let (process, client) = match spec.spawn_connected_with_overlay(
            snapshot.overlay.as_ref(),
            Duration::from_millis(ppc_router::DEFAULT_PLUGIN_PPC_TIMEOUT_MS),
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

fn insert_started_service_processes(
    state: &mut DaemonPluginState,
    started_services: Vec<StartedPluginService>,
) -> Result<usize, DaemonError> {
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

fn acquire_service_snapshot(
    key: &PluginServiceKey,
    reason: &str,
) -> Result<PluginServiceSnapshot, DaemonError> {
    let mut stack = LayerStack::open(PathBuf::from(&key.layer_stack_root))?;
    let lease = stack.acquire_snapshot(&format!(
        "plugin-service:{}:{}:{reason}",
        key.plugin_id, key.service_id
    ))?;
    let mut snapshot = service_snapshot_from_lease(&key.layer_stack_root, lease);
    snapshot.overlay = service_overlay_for_snapshot(key, &snapshot)?;
    Ok(snapshot)
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

#[cfg(all(target_os = "linux", not(test)))]
fn service_overlay_for_snapshot(
    key: &PluginServiceKey,
    snapshot: &PluginServiceSnapshot,
) -> Result<Option<PluginServiceOverlay>, DaemonError> {
    let run_dir = overlay_writable_root()
        .map_err(|err| crate::dispatcher::overlay_daemon_error("overlay writable root", &err))?
        .join("runtime")
        .join("plugin-service")
        .join(format!(
            "{}-{}-{}",
            std::process::id(),
            sanitize_path_component(&key.service_id),
            sanitize_path_component(&snapshot.manifest_key)
        ));
    let dirs = allocate_overlay_writable_dirs(&run_dir)
        .map_err(|err| crate::dispatcher::overlay_daemon_error("allocate overlay dirs", &err))?;
    Ok(Some(PluginServiceOverlay {
        run_dir: dirs.run_dir,
        layer_paths: snapshot.layer_paths.iter().map(PathBuf::from).collect(),
        upperdir: dirs.upperdir,
        workdir: dirs.workdir,
    }))
}

#[cfg(any(not(target_os = "linux"), test))]
// Keep the same fallible signature as Linux so service snapshot setup remains
// cfg-free for callers; off-Linux/test builds do not allocate overlay dirs.
#[expect(
    clippy::unnecessary_wraps,
    reason = "non-Linux/test parity keeps the Linux fallible helper signature"
)]
const fn service_overlay_for_snapshot(
    _key: &PluginServiceKey,
    _snapshot: &PluginServiceSnapshot,
) -> Result<Option<PluginServiceOverlay>, DaemonError> {
    Ok(None)
}

#[cfg(all(target_os = "linux", not(test)))]
fn sanitize_path_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "service".to_owned()
    } else {
        cleaned
    }
}

fn active_manifest_key(layer_stack_root: &str) -> Result<String, DaemonError> {
    let manifest = LayerStack::open(PathBuf::from(layer_stack_root))?.read_active_manifest()?;
    Ok(manifest_key(
        manifest.version,
        &manifest_root_hash(&manifest),
    ))
}

fn manifest_key(version: i64, root_hash: &str) -> String {
    format!("{version}:{root_hash}")
}

fn release_service_snapshot(snapshot: &PluginServiceSnapshot) {
    if let Some(overlay) = &snapshot.overlay {
        let _ = std::fs::remove_dir_all(&overlay.run_dir);
    }
    if let Ok(mut stack) = LayerStack::open(PathBuf::from(&snapshot.layer_stack_root)) {
        let _ = stack.release_lease(&snapshot.lease_id);
    }
}

fn mark_service_ready(
    state: &mut DaemonPluginState,
    service_instance_id: &str,
    snapshot: &PluginServiceSnapshot,
    refreshed: bool,
) -> Result<(), DaemonError> {
    let status = service_status_mut(state, service_instance_id)?;
    status.state = PluginServiceState::Ready;
    status.manifest_key = Some(snapshot.manifest_key.clone());
    if refreshed {
        status.refresh_count = status.refresh_count.saturating_add(1);
    }
    status.last_error = None;
    Ok(())
}

fn mark_service_restarted(
    state: &mut DaemonPluginState,
    service_instance_id: &str,
) -> Result<(), DaemonError> {
    let status = service_status_mut(state, service_instance_id)?;
    status.restart_count = status.restart_count.saturating_add(1);
    Ok(())
}

fn mark_service_stale(
    state: &mut DaemonPluginState,
    service_instance_id: &str,
    reason: impl Into<String>,
) -> Result<(), DaemonError> {
    let status = service_status_mut(state, service_instance_id)?;
    status.state = PluginServiceState::Stale;
    status.last_error = Some(reason.into());
    Ok(())
}

fn mark_service_stopped(state: &mut DaemonPluginState, service_instance_id: &str) {
    if let Ok(status) = service_status_mut(state, service_instance_id) {
        status.state = PluginServiceState::Stopped;
        status.last_error = Some("service process stopped".to_owned());
    }
}

fn service_status_mut<'a>(
    state: &'a mut DaemonPluginState,
    service_instance_id: &str,
) -> Result<&'a mut PluginServiceStatus, DaemonError> {
    state
        .loaded
        .values_mut()
        .flat_map(|loaded| loaded.services.iter_mut())
        .find(|status| status.key.service_instance_id() == service_instance_id)
        .ok_or_else(|| {
            DaemonError::Plugin(PluginError::Ensure(format!(
                "service {service_instance_id} is not registered"
            )))
        })
}

fn service_process_still_declared(state: &DaemonPluginState, service_instance_id: &str) -> bool {
    state.loaded.values().any(|loaded| {
        loaded
            .service_processes
            .iter()
            .any(|spec| spec.service_instance_id() == service_instance_id)
    })
}

fn stop_plugin_service_processes(state: &mut DaemonPluginState, plugin_id: &str) {
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
    for service_instance_id in stale_service_ids {
        state.service_processes.remove(&service_instance_id);
        state.service_ppc_clients.remove(&service_instance_id);
        if let Some(snapshot) = state.service_snapshots.remove(&service_instance_id) {
            release_service_snapshot(&snapshot);
        }
        mark_service_stopped(state, &service_instance_id);
    }
}

pub(crate) fn stop_services_for_layer_stack_root(
    layer_stack_root: &str,
) -> Result<usize, DaemonError> {
    let mut state = lock_state()?;
    let service_instance_ids = state
        .service_snapshots
        .iter()
        .filter_map(|(service_instance_id, snapshot)| {
            (snapshot.layer_stack_root == layer_stack_root).then(|| service_instance_id.clone())
        })
        .collect::<Vec<_>>();
    let stopped_count = service_instance_ids.len();
    for service_instance_id in service_instance_ids {
        state.service_processes.remove(&service_instance_id);
        state.service_ppc_clients.remove(&service_instance_id);
        if let Some(snapshot) = state.service_snapshots.remove(&service_instance_id) {
            release_service_snapshot(&snapshot);
        }
        mark_service_stopped(&mut state, &service_instance_id);
    }
    Ok(stopped_count)
}

fn running_process_values(state: &mut DaemonPluginState) -> Vec<Value> {
    let mut closed = Vec::new();
    let mut values = Vec::new();
    for (service_instance_id, process) in &mut state.service_processes {
        let status = process.status_json();
        if status["running"] != true {
            closed.push(service_instance_id.clone());
        }
        values.push(status);
    }
    for service_instance_id in closed {
        state.service_processes.remove(&service_instance_id);
        state.service_ppc_clients.remove(&service_instance_id);
        if let Some(snapshot) = state.service_snapshots.remove(&service_instance_id) {
            release_service_snapshot(&snapshot);
        }
        mark_service_stopped(state, &service_instance_id);
    }
    values
}

fn loaded_plugin_values(state: &DaemonPluginState) -> Vec<Value> {
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

#[derive(Debug, Clone)]
struct ServiceHealthProbeTarget {
    plugin_id: String,
    service_id: String,
    service_instance_id: String,
    manifest_key: String,
    client: SharedPpcClient,
}

fn service_health_probe_targets(state: &DaemonPluginState) -> Vec<ServiceHealthProbeTarget> {
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

fn probe_service_health(targets: Vec<ServiceHealthProbeTarget>, timeout: Duration) -> Vec<Value> {
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

fn route_for_op(op: &str) -> Result<Option<PluginOperationRoute>, DaemonError> {
    let state = lock_state()?;
    Ok(state
        .loaded
        .values()
        .find_map(|loaded| loaded.operation_routes.get(op).cloned()))
}

fn dispatch_registered_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Value, DaemonError> {
    ensure_plugin_family_allowed(args)?;
    if route.intent == Intent::ReadOnly && route.service_id.is_some() {
        if let Some(response) = dispatch_connected_read_only_route(route, invocation_id, args)? {
            return Ok(response);
        }
    }
    if route.intent == Intent::WriteAllowed && route.auto_workspace_overlay {
        if let Some(response) = dispatch_oneshot_overlay_route(route, invocation_id, args)? {
            return Ok(response);
        }
    }
    if route.intent == Intent::WriteAllowed
        && !route.auto_workspace_overlay
        && route.service_id.is_some()
    {
        if let Some(response) = dispatch_connected_self_managed_route(route, invocation_id, args)? {
            return Ok(response);
        }
    }
    dispatch_deferred_route(route, args)
}

fn dispatch_connected_read_only_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Option<Value>, DaemonError> {
    let Some(service_instance_id) = route.service_instance_id.clone() else {
        return Ok(None);
    };
    let Some(client) = ensure_connected_service_current(route, invocation_id)? else {
        return Ok(None);
    };
    let timeout = Duration::from_millis(
        route
            .timeout_ms
            .unwrap_or(ppc_router::DEFAULT_PLUGIN_PPC_TIMEOUT_MS),
    );
    let request = PpcEnvelope {
        message_id: invocation_id.to_owned(),
        direction: PpcDirection::Request,
        op: route.public_op.clone(),
        body: serde_json::to_string(args).map_err(|err| PluginError::Ppc(err.to_string()))?,
    };
    let reply = client.round_trip(&request, timeout);
    let reply = match reply {
        Ok(reply) => reply,
        Err(err) => {
            teardown_failed_connected_service(&service_instance_id, &err.to_string())?;
            return Err(err);
        }
    };
    response_payload_from_reply(&reply)
}

fn ensure_connected_service_current(
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

    // Refresh mutates the service namespace and snapshot lease, so it is
    // singleflight per service. Operation dispatch remains multiplexed after
    // this freshness gate returns.
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
    Ok(state
        .loaded
        .values()
        .flat_map(|loaded| loaded.services.iter())
        .find(|status| status.key.service_instance_id() == service_instance_id)
        .is_some_and(|status| status.manifest_key.is_some()))
}

fn service_is_ready_on_manifest(
    service_instance_id: &str,
    target_manifest_key: &str,
) -> Result<bool, DaemonError> {
    let state = lock_state()?;
    Ok(state
        .loaded
        .values()
        .flat_map(|loaded| loaded.services.iter())
        .find(|status| status.key.service_instance_id() == service_instance_id)
        .is_some_and(|status| {
            status
                .require_ready_on_manifest(target_manifest_key)
                .is_ok()
        }))
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
            .unwrap_or(ppc_router::DEFAULT_PLUGIN_PPC_TIMEOUT_MS),
    );
    let refresh_result = {
        send_refresh_sequence(
            client,
            service_key,
            service_instance_id,
            invocation_id,
            &snapshot,
            timeout,
        )
    };
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
    client: &ppc_router::PpcClient,
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
    client: &ppc_router::PpcClient,
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

fn dispatch_oneshot_overlay_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Option<Value>, DaemonError> {
    if route.service_mode != Some(ServiceMode::OneshotOverlay) {
        return Ok(None);
    }
    let Some(layer_stack_root) = route.layer_stack_root.clone() else {
        return Ok(None);
    };
    let Some(service_key) = route.service_key.clone() else {
        return Ok(None);
    };
    if route.service_command.is_empty() {
        return Ok(None);
    }
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_owned();
    let mut env = BTreeMap::from([
        (
            "EOS_PLUGIN_LAYER_STACK_ROOT".to_owned(),
            service_key.layer_stack_root,
        ),
        (
            "EOS_PLUGIN_WORKSPACE_ROOT".to_owned(),
            service_key.workspace_root,
        ),
        ("EOS_PLUGIN_ID".to_owned(), service_key.plugin_id),
        ("EOS_PLUGIN_DIGEST".to_owned(), service_key.plugin_digest),
        ("EOS_PLUGIN_SERVICE_ID".to_owned(), service_key.service_id),
        (
            "EOS_PLUGIN_SERVICE_PROFILE_DIGEST".to_owned(),
            service_key.service_profile_digest,
        ),
        (
            "EOS_PLUGIN_PPC_PROTOCOL_VERSION".to_owned(),
            route.service_ppc_protocol_version.unwrap_or(1).to_string(),
        ),
        (
            "EOS_PLUGIN_SERVICE_MODE".to_owned(),
            "oneshot_overlay".to_owned(),
        ),
    ]);
    env.insert("EOS_PLUGIN_PUBLIC_OP".to_owned(), route.public_op.clone());
    let timeout_seconds = route
        .timeout_ms
        .map(|timeout| crate::dispatcher::u64_to_f64_saturating(timeout) / 1000.0);
    let overlay_command = PluginOverlayCommand {
        layer_stack_root: PathBuf::from(layer_stack_root),
        invocation_id: invocation_id.to_owned(),
        agent_id,
        public_op: route.public_op.clone(),
        plugin_id: route.plugin_id.clone(),
        op_name: route.op_name.clone(),
        command: route.service_command.clone(),
        env,
        timeout_seconds,
    };
    Ok(Some(crate::dispatcher::run_plugin_overlay_command(
        &overlay_command,
        args,
        Instant::now(),
    )?))
}

fn dispatch_connected_self_managed_route(
    route: &PluginOperationRoute,
    invocation_id: &str,
    args: &Value,
) -> Result<Option<Value>, DaemonError> {
    let Some(service_instance_id) = route.service_instance_id.clone() else {
        return Ok(None);
    };
    let Some(layer_stack_root) = route.layer_stack_root.clone() else {
        return Ok(None);
    };
    let Some(client) = ensure_connected_service_current(route, invocation_id)? else {
        return Ok(None);
    };
    let timeout = Duration::from_millis(
        route
            .timeout_ms
            .unwrap_or(ppc_router::DEFAULT_PLUGIN_PPC_TIMEOUT_MS),
    );
    let request = PpcEnvelope {
        message_id: invocation_id.to_owned(),
        direction: PpcDirection::Request,
        op: route.public_op.clone(),
        body: serde_json::to_string(args).map_err(|err| PluginError::Ppc(err.to_string()))?,
    };
    let expected_root = PathBuf::from(layer_stack_root);
    let reply = client.round_trip_with_callbacks(&request, timeout, move |callback| {
        occ_callbacks::handle_callback_for_root(&expected_root, callback)
    });
    let reply = match reply {
        Ok(reply) => reply,
        Err(err) => {
            teardown_failed_connected_service(&service_instance_id, &err.to_string())?;
            return Err(err);
        }
    };
    response_payload_from_reply(&reply)
}

fn response_payload_from_reply(reply: &PpcEnvelope) -> Result<Option<Value>, DaemonError> {
    let payload: Value =
        serde_json::from_str(&reply.body).map_err(|err| PluginError::Ppc(err.to_string()))?;
    if payload.is_object() {
        Ok(Some(payload))
    } else {
        Ok(Some(json!({
            "success": true,
            "result": payload,
        })))
    }
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

fn teardown_failed_connected_service(
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

fn dispatch_deferred_route(
    route: &PluginOperationRoute,
    args: &Value,
) -> Result<Value, DaemonError> {
    ensure_plugin_family_allowed(args)?;
    Ok(json!({
        "success": false,
        "status": "deferred",
        "op": route.public_op,
        "plugin": route.plugin_id,
        "op_name": route.op_name,
        "intent": route.intent,
        "auto_workspace_overlay": route.auto_workspace_overlay,
        "service_id": route.service_id,
        "dispatch_mode": route.dispatch_mode(),
        "error": {
            "kind": "plugin_dispatch_deferred",
            "message": "plugin service is not connected for this route",
            "details": {
                "op": route.public_op,
                "dispatch_mode": route.dispatch_mode(),
            },
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::OpTable;
    use eos_protocol::Request;
    use std::error::Error;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    type TestError = Box<dyn Error + Send + Sync + 'static>;
    type TestResult = Result<(), TestError>;

    struct PluginTestGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl PluginTestGuard {
        fn new() -> Result<Self, TestError> {
            let guard = TEST_LOCK
                .lock()
                .map_err(|_| std::io::Error::other("plugin test lock poisoned"))?;
            reset_for_tests();
            Ok(Self { _guard: guard })
        }
    }

    impl Drop for PluginTestGuard {
        fn drop(&mut self) {
            reset_for_tests();
        }
    }

    struct TestEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl TestEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for TestEnvVar {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn value_array<'a>(
        value: &'a Value,
        context: &'static str,
    ) -> Result<&'a Vec<Value>, TestError> {
        value
            .as_array()
            .ok_or_else(|| std::io::Error::other(context).into())
    }

    fn value_str<'a>(value: &'a Value, context: &'static str) -> Result<&'a str, TestError> {
        value
            .as_str()
            .ok_or_else(|| std::io::Error::other(context).into())
    }

    fn some_value<T>(value: Option<T>, context: &'static str) -> Result<T, TestError> {
        value.ok_or_else(|| std::io::Error::other(context).into())
    }

    fn ppc_stream_pair() -> Result<
        (
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
        ),
        TestError,
    > {
        Ok(std::os::unix::net::UnixStream::pair()?)
    }

    fn read_ppc_request(
        stream: &mut std::os::unix::net::UnixStream,
        context: &'static str,
    ) -> Result<PpcEnvelope, TestError> {
        let frame = ppc_router::read_frame(stream)?;
        PpcEnvelope::decode(&frame)
            .map_err(|err| std::io::Error::other(format!("{context}: {err}")).into())
    }

    fn write_ppc_reply_json_result(
        stream: &mut std::os::unix::net::UnixStream,
        message_id: String,
        body: &Value,
    ) -> TestResult {
        let reply = PpcEnvelope {
            message_id,
            direction: PpcDirection::Reply,
            op: "reply".to_owned(),
            body: serde_json::to_string(body)?,
        };
        stream.write_all(&reply.encode()?)?;
        Ok(())
    }

    fn write_ppc_reply_result(
        stream: &mut std::os::unix::net::UnixStream,
        message_id: String,
        body: &'static str,
    ) -> TestResult {
        let reply = PpcEnvelope {
            message_id,
            direction: PpcDirection::Reply,
            op: "reply".to_owned(),
            body: body.to_owned(),
        };
        stream.write_all(&reply.encode()?)?;
        Ok(())
    }

    fn join_test_thread(
        handle: std::thread::JoinHandle<TestResult>,
        context: &'static str,
    ) -> TestResult {
        handle
            .join()
            .map_err(|_| std::io::Error::other(context))??;
        Ok(())
    }

    fn join_value_thread(
        handle: std::thread::JoinHandle<Result<Value, TestError>>,
        context: &'static str,
    ) -> Result<Value, TestError> {
        handle.join().map_err(|_| std::io::Error::other(context))?
    }

    fn lsp_manifest(digest: &str, op_name: &str) -> Value {
        json!({
            "plugin_id": "lsp",
            "plugin_version": "0.1.0",
            "plugin_digest": digest,
            "services": [{
                "service_id": "pyright",
                "service_profile_digest": format!("profile-{digest}"),
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace_and_notify",
                "command": ["pyright-langserver", "--stdio"],
                "ppc_protocol_version": 1
            }],
            "operations": [{
                "op_name": op_name,
                "intent": "read_only",
                "service_id": "pyright"
            }]
        })
    }

    fn lsp_manifest_with_command(digest: &str, op_name: &str, command: Vec<&str>) -> Value {
        let mut manifest = lsp_manifest(digest, op_name);
        manifest["services"][0]["command"] =
            Value::Array(command.into_iter().map(|item| json!(item)).collect());
        manifest
    }

    fn lsp_restart_manifest(digest: &str, op_name: &str, command: Vec<&str>) -> Value {
        let mut manifest = lsp_manifest_with_command(digest, op_name, command);
        manifest["services"][0]["refresh_strategy"] = json!("restart_service");
        manifest
    }

    fn lsp_self_managed_manifest(digest: &str, op_name: &str) -> Value {
        let mut manifest = lsp_manifest(digest, op_name);
        manifest["operations"][0]["intent"] = json!("write_allowed");
        manifest["operations"][0]["auto_workspace_overlay"] = json!(false);
        manifest
    }

    fn oneshot_overlay_manifest(digest: &str, op_name: &str) -> Value {
        json!({
            "plugin_id": "generic",
            "plugin_version": "0.1.0",
            "plugin_digest": digest,
            "services": [{
                "service_id": "worker",
                "service_profile_digest": format!("oneshot-profile-{digest}"),
                "service_mode": "oneshot_overlay",
                "refresh_strategy": "restart_service",
                "command": ["python3", "/eos/plugin/oneshot.py"],
                "ppc_protocol_version": 1
            }],
            "operations": [{
                "op_name": op_name,
                "intent": "write_allowed",
                "service_id": "worker",
                "timeout_ms": 5000
            }]
        })
    }

    #[test]
    fn ensure_records_manifest_services_and_status_lists_them() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let response = op_ensure(
            &json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/eos/plugin/workspace"
            }),
            DispatchContext::empty(),
        )?;
        assert_eq!(response["success"], true);
        assert_eq!(response["registered_ops"], json!(["plugin.lsp.hover"]));
        assert_eq!(
            response["operation_routes"][0]["dispatch_mode"],
            "read_only_service"
        );
        assert_eq!(response["services"][0]["state"], "stopped");
        assert_eq!(response["service_processes"][0]["service_id"], "pyright");
        assert!(value_str(
            &response["service_processes"][0]["socket_path"],
            "socket path must be a string"
        )?
        .starts_with("/eos/plugin/ppc/"));

        let status = op_status(&json!({}), DispatchContext::empty())?;
        assert_eq!(status["loaded_plugins"][0]["name"], "lsp");
        Ok(())
    }

    #[test]
    fn ensure_is_idempotent_for_same_digest() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let first = op_ensure(
            &json!({"plugin": "demo", "digest": "a"}),
            DispatchContext::empty(),
        )?;
        let second = op_ensure(
            &json!({"plugin": "demo", "digest": "a"}),
            DispatchContext::empty(),
        )?;
        assert_eq!(first["already_loaded"], false);
        assert_eq!(second["already_loaded"], true);
        Ok(())
    }

    #[test]
    fn ensure_reloads_same_digest_when_workspace_root_changes() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let first = op_ensure(
            &json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/testbed"
            }),
            DispatchContext::empty(),
        )?;
        let second = op_ensure(
            &json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/ephemeral-os"
            }),
            DispatchContext::empty(),
        )?;

        assert_eq!(first["already_loaded"], false);
        assert_eq!(second["already_loaded"], false);
        assert_eq!(
            first["service_processes"][0]["env"]["EOS_PLUGIN_WORKSPACE_ROOT"],
            "/testbed"
        );
        assert_eq!(
            second["service_processes"][0]["env"]["EOS_PLUGIN_WORKSPACE_ROOT"],
            "/ephemeral-os"
        );
        Ok(())
    }

    #[test]
    fn build_workspace_base_reset_stops_plugin_service_snapshots_for_layer_root() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("reset-plugin-service")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-reset-service".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);
        let _ = attach_service_snapshot_for_tests("plugin.lsp.hover")?;
        assert_eq!(
            LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
            1
        );

        let reset = table.dispatch(&Request {
            op: "api.build_workspace_base".to_owned(),
            invocation_id: "workspace-base-reset-stops-service".to_owned(),
            args: json!({
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned(),
                "reset": true
            }),
        });

        assert_eq!(reset["success"], true, "{reset:?}");
        assert_eq!(
            LayerStack::open(layer_stack_root.clone())?.active_lease_count(),
            0
        );
        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-reset-service".to_owned(),
            args: json!({}),
        });
        assert_eq!(
            status["loaded_plugins"][0]["services"][0]["state"],
            "stopped"
        );
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn op_table_registers_plugin_status_and_ensure() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({"plugin": "demo", "digest": "a"}),
        });
        assert_eq!(ensure["success"], true);

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-test".to_owned(),
            args: json!({}),
        });
        assert_eq!(status["success"], true);
        let loaded = value_array(&status["loaded_plugins"], "loaded_plugins must be an array")?;
        assert!(loaded.iter().any(|plugin| plugin["name"] == "demo"));
        Ok(())
    }

    #[test]
    fn registered_plugin_op_routes_to_deferred_dispatch_not_unknown_op() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/eos/plugin/workspace"
            }),
        });
        assert_eq!(ensure["success"], true);

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-test".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], false);
        assert_eq!(routed["status"], "deferred");
        assert_eq!(routed["error"]["kind"], "plugin_dispatch_deferred");
        assert_eq!(routed["dispatch_mode"], "read_only_service");

        let missing = table.dispatch(&Request {
            op: "plugin.lsp.missing".to_owned(),
            invocation_id: "plugin-missing-test".to_owned(),
            args: json!({}),
        });
        assert_eq!(missing["error"]["kind"], "unknown_op");
        Ok(())
    }

    #[test]
    fn dynamic_plugin_op_is_blocked_in_isolated_workspace_before_route_lookup() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, _workspace_root) = test_bound_workspace("plugin-iws-block")?;
        let scratch = some_value(
            layer_stack_root.parent(),
            "test layer root must have a parent",
        )?
        .join("scratch");
        let _enabled = TestEnvVar::set("EOS_ISOLATED_WORKSPACE_ENABLED", "true");
        let _harness = TestEnvVar::set("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");
        let _scratch = TestEnvVar::set(
            "EOS_ISOLATED_WORKSPACE_TEST_SCRATCH_ROOT",
            &scratch.to_string_lossy(),
        );

        let _ = table.dispatch(&Request {
            op: "api.isolated_workspace.test_reset".to_owned(),
            invocation_id: "iws-reset-before-plugin-block".to_owned(),
            args: json!({}),
        });
        let entered = table.dispatch(&Request {
            op: "api.isolated_workspace.enter".to_owned(),
            invocation_id: "iws-enter-before-plugin-block".to_owned(),
            args: json!({
                "agent_id": "agent-plugin",
                "layer_stack_root": layer_stack_root.to_string_lossy(),
            }),
        });
        assert_eq!(entered["success"], true);

        let blocked = table.dispatch(&Request {
            op: "plugin.lsp.not_loaded_yet".to_owned(),
            invocation_id: "plugin-dynamic-iws-block".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(blocked["error"]["kind"], "forbidden_in_isolated_workspace");

        let exited = table.dispatch(&Request {
            op: "api.isolated_workspace.exit".to_owned(),
            invocation_id: "iws-exit-after-plugin-block".to_owned(),
            args: json!({"agent_id": "agent-plugin", "force_cancel": true}),
        });
        assert_eq!(exited["success"], true);
        let _ = table.dispatch(&Request {
            op: "api.isolated_workspace.test_reset".to_owned(),
            invocation_id: "iws-reset-after-plugin-block".to_owned(),
            args: json!({}),
        });
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn exited_service_process_fails_closed_before_dispatch() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("exited-service")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-exited-service".to_owned(),
            args: json!({
                "manifest": lsp_manifest_with_command(
                    "digest-a",
                    "hover",
                    vec!["/bin/sh", "-c", "true"]
                ),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let spec = {
            let state = lock_state()?;
            some_value(
                state
                    .loaded
                    .values()
                    .flat_map(|loaded| loaded.service_processes.iter())
                    .next()
                    .cloned(),
                "service process spec missing",
            )?
        };
        let service_instance_id = spec.service_instance_id();
        let process = spec.spawn()?;
        std::thread::sleep(Duration::from_millis(50));
        {
            let mut state = lock_state()?;
            state.service_processes.insert(service_instance_id, process);
        }

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-exited-service".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], false);
        assert!(
            value_str(
                &routed["error"]["message"],
                "error message must be a string"
            )?
            .contains("process exited before plugin dispatch"),
            "routed response: {routed:?}"
        );

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-exited-service".to_owned(),
            args: json!({}),
        });
        assert_eq!(
            status["loaded_plugins"][0]["services"][0]["state"],
            "stopped"
        );
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn ensure_records_oneshot_overlay_route_without_starting_process() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let response = op_ensure(
            &json!({
                "manifest": oneshot_overlay_manifest("digest-a", "write"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/eos/plugin/workspace",
                "start_services": true
            }),
            DispatchContext::empty(),
        )?;

        assert_eq!(response["success"], true);
        assert_eq!(response["service_processes"], json!([]));
        assert_eq!(response["service_processes_started"], false);
        assert_eq!(
            response["operation_routes"][0]["dispatch_mode"],
            "write_allowed_oneshot_overlay"
        );
        assert_eq!(
            response["operation_routes"][0]["service_mode"],
            "oneshot_overlay"
        );
        assert_eq!(
            response["operation_routes"][0]["service_command"],
            json!(["python3", "/eos/plugin/oneshot.py"])
        );
        assert_eq!(
            response["services"][0]["last_error"],
            "oneshot overlay worker starts per operation"
        );
        Ok(())
    }

    #[test]
    fn digest_reload_replaces_dynamic_plugin_routes() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let first = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-a".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/eos/plugin/workspace"
            }),
        });
        assert_eq!(first["registered_ops"], json!(["plugin.lsp.hover"]));

        let second = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-b".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-b", "diagnostics"),
                "layer_stack_root": "/eos/plugin/layer-stack",
                "workspace_root": "/eos/plugin/workspace"
            }),
        });
        assert_eq!(second["registered_ops"], json!(["plugin.lsp.diagnostics"]));

        let old = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-old".to_owned(),
            args: json!({}),
        });
        assert_eq!(old["error"]["kind"], "unknown_op");

        let current = table.dispatch(&Request {
            op: "plugin.lsp.diagnostics".to_owned(),
            invocation_id: "plugin-diagnostics-current".to_owned(),
            args: json!({}),
        });
        assert_eq!(current["error"]["kind"], "plugin_dispatch_deferred");
        Ok(())
    }

    #[test]
    fn connected_read_only_plugin_op_round_trips_over_ppc() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("read-only-ppc")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        let server = std::thread::spawn(move || -> TestResult {
            let request = read_ppc_request(&mut server_stream, "read ppc request")?;
            assert_eq!(request.message_id, "plugin-hover-test");
            assert_eq!(request.op, "plugin.lsp.hover");
            assert!(request.body.contains("agent-plugin"));
            let reply = PpcEnvelope {
                message_id: request.message_id,
                direction: PpcDirection::Reply,
                op: "reply".to_owned(),
                body: r#"{"success":true,"from_ppc":true}"#.to_owned(),
            };
            server_stream.write_all(&reply.encode()?)?;
            Ok(())
        });

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-test".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], true);
        assert_eq!(routed["from_ppc"], true);
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn status_probe_services_sends_health_request() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("status-health-ok")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-health-ok".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        let (_service_instance_id, manifest_key) =
            attach_service_snapshot_for_tests("plugin.lsp.hover")?;
        let expected_manifest_key = manifest_key.clone();
        let server = std::thread::spawn(move || -> TestResult {
            let request = read_ppc_request(&mut server_stream, "read health request")?;
            assert_eq!(request.op, WORKSPACE_SNAPSHOT_REFRESH_OP);
            let body: Value = serde_json::from_str(&request.body)?;
            assert_eq!(body["type"], "health");
            assert_eq!(body["manifest_key"], expected_manifest_key);
            write_ppc_reply_json_result(
                &mut server_stream,
                request.message_id,
                &json!({"manifest_key": expected_manifest_key, "accepted": true}),
            )?;
            Ok(())
        });

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-health-ok".to_owned(),
            args: json!({"probe_services": true, "probe_timeout_ms": 1000}),
        });
        assert_eq!(status["success"], true);
        assert_eq!(status["service_health"][0]["success"], true);
        assert_eq!(status["service_health"][0]["service_id"], "pyright");
        assert_eq!(status["service_health"][0]["manifest_key"], manifest_key);
        assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
        assert_eq!(status["connected_ppc_routes"], json!(["plugin.lsp.hover"]));
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn status_probe_failure_drops_connected_service() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("status-health-fail")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-health-fail".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        let (service_instance_id, manifest_key) =
            attach_service_snapshot_for_tests("plugin.lsp.hover")?;
        let server = std::thread::spawn(move || -> TestResult {
            let request = read_ppc_request(&mut server_stream, "read health request")?;
            assert_eq!(request.op, WORKSPACE_SNAPSHOT_REFRESH_OP);
            write_ppc_reply_json_result(
                &mut server_stream,
                request.message_id,
                &json!({"manifest_key": "wrong-manifest", "accepted": true}),
            )?;
            Ok(())
        });

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-health-fail".to_owned(),
            args: json!({"probe_services": true, "probe_timeout_ms": 1000}),
        });
        assert_eq!(status["success"], true);
        assert_eq!(status["service_health"][0]["success"], false);
        assert!(
            value_str(
                &status["service_health"][0]["error"],
                "probe error must be a string"
            )?
            .contains(&manifest_key),
            "status response: {status:?}"
        );
        assert_eq!(status["connected_ppc_routes"], json!([]));
        assert_eq!(status["connected_ppc_services"], json!([]));
        assert_eq!(
            status["loaded_plugins"][0]["services"][0]["state"],
            "stopped"
        );
        {
            let state = lock_state()?;
            assert!(
                !state.service_snapshots.contains_key(&service_instance_id),
                "failed health probe should release retained snapshot"
            );
            drop(state);
        }
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn read_only_service_refreshes_after_peer_publish_before_request() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("read-only-refresh")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;

        let write = table.dispatch(&Request {
            op: "api.v1.write_file".to_owned(),
            invocation_id: "peer-write".to_owned(),
            args: json!({
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "path": workspace_root.join("peer.txt").to_string_lossy().into_owned(),
                "content": "peer\n"
            }),
        });
        assert_eq!(write["success"], true, "write response: {write:?}");

        let server = std::thread::spawn(move || -> TestResult {
            let mut refresh_types = Vec::new();
            let mut current_manifest_key = String::new();
            loop {
                let request = read_ppc_request(&mut server_stream, "read ppc request")?;
                if request.op == WORKSPACE_SNAPSHOT_REFRESH_OP {
                    let body: Value = serde_json::from_str(&request.body)?;
                    refresh_types.push(
                        value_str(&body["type"], "refresh type must be a string")?.to_owned(),
                    );
                    if let Some(key) = body
                        .get("target_manifest_key")
                        .or_else(|| body.get("manifest_key"))
                        .and_then(Value::as_str)
                    {
                        current_manifest_key = key.to_owned();
                    }
                    let refresh_reply = json!({
                            "manifest_key": current_manifest_key,
                            "accepted": true
                    });
                    write_ppc_reply_json_result(
                        &mut server_stream,
                        request.message_id,
                        &refresh_reply,
                    )?;
                    continue;
                }

                assert_eq!(request.message_id, "plugin-hover-after-peer-write");
                assert_eq!(request.op, "plugin.lsp.hover");
                assert!(refresh_types.contains(&"prepare_refresh".to_owned()));
                assert!(refresh_types.contains(&"swap_workspace".to_owned()));
                assert!(refresh_types.contains(&"health".to_owned()));
                write_ppc_reply_result(
                    &mut server_stream,
                    request.message_id,
                    r#"{"success":true,"after_refresh":true}"#,
                )?;
                break Ok(());
            }
        });

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-after-peer-write".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], true, "routed response: {routed:?}");
        assert_eq!(routed["after_refresh"], true);

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-refresh".to_owned(),
            args: json!({}),
        });
        assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
        assert_eq!(
            status["loaded_plugins"][0]["services"][0]["refresh_count"],
            1
        );
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn concurrent_read_only_refresh_is_singleflight_before_requests() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = Arc::new(OpTable::with_builtins());
        let (layer_stack_root, workspace_root) =
            test_bound_workspace("read-only-refresh-singleflight")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;

        let write = table.dispatch(&Request {
            op: "api.v1.write_file".to_owned(),
            invocation_id: "peer-write".to_owned(),
            args: json!({
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "path": workspace_root.join("peer.txt").to_string_lossy().into_owned(),
                "content": "peer\n"
            }),
        });
        assert_eq!(write["success"], true, "write response: {write:?}");

        let (refresh_started_tx, refresh_started_rx) = mpsc::channel();
        let (continue_refresh_tx, continue_refresh_rx) = mpsc::channel();
        let server = std::thread::spawn(move || -> TestResult {
            let mut refresh_types = Vec::new();
            let mut current_manifest_key = String::new();
            let first_op = loop {
                let request = read_ppc_request(&mut server_stream, "read ppc request")?;
                if request.op != WORKSPACE_SNAPSHOT_REFRESH_OP {
                    break request;
                }
                let body: Value = serde_json::from_str(&request.body)?;
                let refresh_type =
                    value_str(&body["type"], "refresh type must be a string")?.to_owned();
                if refresh_types.is_empty() {
                    assert_eq!(refresh_type, "prepare_refresh");
                    refresh_started_tx.send(())?;
                    continue_refresh_rx.recv_timeout(Duration::from_secs(1))?;
                }
                refresh_types.push(refresh_type);
                if let Some(key) = body
                    .get("target_manifest_key")
                    .or_else(|| body.get("manifest_key"))
                    .and_then(Value::as_str)
                {
                    current_manifest_key = key.to_owned();
                }
                let refresh_reply = json!({
                    "manifest_key": current_manifest_key,
                    "accepted": true
                });
                write_ppc_reply_json_result(
                    &mut server_stream,
                    request.message_id,
                    &refresh_reply,
                )?;
            };
            assert_eq!(
                refresh_types,
                vec![
                    "prepare_refresh".to_owned(),
                    "quiesce".to_owned(),
                    "swap_workspace".to_owned(),
                    "notify_refresh".to_owned(),
                    "resume".to_owned(),
                    "health".to_owned(),
                ]
            );

            let second_op = read_ppc_request(&mut server_stream, "read second plugin request")?;
            let mut message_ids = vec![first_op.message_id.clone(), second_op.message_id.clone()];
            message_ids.sort();
            assert_eq!(
                message_ids,
                vec![
                    "plugin-hover-concurrent-refresh-a".to_owned(),
                    "plugin-hover-concurrent-refresh-b".to_owned(),
                ]
            );
            assert_eq!(first_op.op, "plugin.lsp.hover");
            assert_eq!(second_op.op, "plugin.lsp.hover");
            write_ppc_reply_result(
                &mut server_stream,
                second_op.message_id,
                r#"{"success":true,"seq":2}"#,
            )?;
            write_ppc_reply_result(
                &mut server_stream,
                first_op.message_id,
                r#"{"success":true,"seq":1}"#,
            )?;
            Ok(())
        });

        let first_table = Arc::clone(&table);
        let first = std::thread::spawn(move || -> Result<Value, TestError> {
            Ok(first_table.dispatch(&Request {
                op: "plugin.lsp.hover".to_owned(),
                invocation_id: "plugin-hover-concurrent-refresh-a".to_owned(),
                args: json!({"agent_id": "agent-plugin", "request": "a"}),
            }))
        });
        refresh_started_rx.recv_timeout(Duration::from_secs(1))?;

        let (second_started_tx, second_started_rx) = mpsc::channel();
        let second_table = Arc::clone(&table);
        let second = std::thread::spawn(move || -> Result<Value, TestError> {
            second_started_tx.send(())?;
            Ok(second_table.dispatch(&Request {
                op: "plugin.lsp.hover".to_owned(),
                invocation_id: "plugin-hover-concurrent-refresh-b".to_owned(),
                args: json!({"agent_id": "agent-plugin", "request": "b"}),
            }))
        });
        second_started_rx.recv_timeout(Duration::from_secs(1))?;
        continue_refresh_tx.send(())?;

        let first_response = join_value_thread(first, "first dispatch thread panicked")?;
        let second_response = join_value_thread(second, "second dispatch thread panicked")?;
        assert_eq!(first_response["success"], true);
        assert_eq!(second_response["success"], true);

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-refresh-singleflight".to_owned(),
            args: json!({}),
        });
        assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
        assert_eq!(
            status["loaded_plugins"][0]["services"][0]["refresh_count"],
            1
        );
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn restart_service_strategy_restarts_after_peer_publish_before_request() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let socket_root = test_socket_root("restart-service");
        let (layer_stack_root, workspace_root) = test_bound_workspace("restart-service")?;
        let (allow_reconnect_tx, allow_reconnect_rx) = mpsc::channel();
        let connector = spawn_restart_connector(
            socket_root.clone(),
            allow_reconnect_rx,
            r#"{"success":true,"from_restart_service":true}"#,
        );
        let command = vec![
            "/bin/sh",
            "-c",
            "test \"$EOS_PLUGIN_SERVICE_ID\" = pyright && sleep 30",
        ];
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-restart-service".to_owned(),
            args: json!({
                "manifest": lsp_restart_manifest("digest-a", "hover", command),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned(),
                "ppc_socket_root": socket_root.to_string_lossy().into_owned(),
                "start_services": true
            }),
        });
        assert_eq!(ensure["success"], true, "ensure response: {ensure:?}");
        assert_eq!(ensure["service_processes_started"], true);

        let status_before = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-before-restart".to_owned(),
            args: json!({}),
        });
        assert_eq!(
            status_before["loaded_plugins"][0]["services"][0]["restart_count"],
            0
        );
        let initial_manifest_key = value_str(
            &status_before["loaded_plugins"][0]["services"][0]["manifest_key"],
            "initial manifest key must be a string",
        )?
        .to_owned();

        let write = table.dispatch(&Request {
            op: "api.v1.write_file".to_owned(),
            invocation_id: "peer-write-before-restart".to_owned(),
            args: json!({
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "path": workspace_root.join("restart-peer.txt").to_string_lossy().into_owned(),
                "content": "peer restart\n"
            }),
        });
        assert_eq!(write["success"], true, "write response: {write:?}");
        allow_reconnect_tx.send(())?;

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-after-restart".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], true, "routed response: {routed:?}");
        assert_eq!(routed["from_restart_service"], true);

        let status_after = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-restart".to_owned(),
            args: json!({}),
        });
        let service = &status_after["loaded_plugins"][0]["services"][0];
        assert_eq!(service["state"], "ready");
        assert_eq!(service["refresh_count"], 0);
        assert_eq!(service["restart_count"], 1);
        assert_ne!(
            value_str(
                &service["manifest_key"],
                "restarted manifest key must be a string"
            )?,
            initial_manifest_key
        );

        join_test_thread(connector, "connector thread panicked")?;
        let _ = std::fs::remove_dir_all(socket_root);
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn concurrent_read_only_plugin_ops_share_one_ppc_client() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = Arc::new(OpTable::with_builtins());
        let (layer_stack_root, workspace_root) = test_bound_workspace("concurrent-read-only")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        let (first_seen_tx, first_seen_rx) = mpsc::channel();
        let (second_seen_tx, second_seen_rx) = mpsc::channel();
        let (reply_first_tx, reply_first_rx) = mpsc::channel();
        let server = std::thread::spawn(move || -> TestResult {
            let first = read_ppc_request(&mut server_stream, "read first ppc request")?;
            first_seen_tx.send(first.message_id.clone())?;
            let second = read_ppc_request(&mut server_stream, "read second ppc request")?;
            second_seen_tx.send(second.message_id.clone())?;
            reply_first_rx.recv()?;
            write_ppc_reply_result(
                &mut server_stream,
                second.message_id,
                r#"{"success":true,"seq":2}"#,
            )?;
            write_ppc_reply_result(
                &mut server_stream,
                first.message_id,
                r#"{"success":true,"seq":1}"#,
            )?;
            Ok(())
        });

        let first_table = Arc::clone(&table);
        let first = std::thread::spawn(move || -> Result<Value, TestError> {
            Ok(first_table.dispatch(&Request {
                op: "plugin.lsp.hover".to_owned(),
                invocation_id: "plugin-hover-concurrent-a".to_owned(),
                args: json!({"agent_id": "agent-plugin", "request": "a"}),
            }))
        });
        assert_eq!(
            first_seen_rx.recv_timeout(Duration::from_secs(1))?,
            "plugin-hover-concurrent-a"
        );

        let (second_started_tx, second_started_rx) = mpsc::channel();
        let second_table = Arc::clone(&table);
        let second = std::thread::spawn(move || -> Result<Value, TestError> {
            second_started_tx.send(())?;
            Ok(second_table.dispatch(&Request {
                op: "plugin.lsp.hover".to_owned(),
                invocation_id: "plugin-hover-concurrent-b".to_owned(),
                args: json!({"agent_id": "agent-plugin", "request": "b"}),
            }))
        });
        second_started_rx.recv_timeout(Duration::from_secs(1))?;
        assert_eq!(
            second_seen_rx.recv_timeout(Duration::from_secs(1))?,
            "plugin-hover-concurrent-b"
        );
        reply_first_tx.send(())?;

        let first_response = join_value_thread(first, "first dispatch thread panicked")?;
        let second_response = join_value_thread(second, "second dispatch thread panicked")?;
        assert_eq!(first_response["success"], true);
        assert_eq!(first_response["seq"], 1);
        assert_eq!(second_response["success"], true);
        assert_eq!(second_response["seq"], 2);
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn concurrent_read_only_plugin_ops_match_out_of_order_replies() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = Arc::new(OpTable::with_builtins());
        let (layer_stack_root, workspace_root) =
            test_bound_workspace("concurrent-read-only-out-of-order")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        let (both_seen_tx, both_seen_rx) = mpsc::channel();
        let server = std::thread::spawn(move || -> TestResult {
            let first = read_ppc_request(&mut server_stream, "read first ppc request")?;
            let second = read_ppc_request(&mut server_stream, "read second ppc request")?;
            let mut message_ids = vec![first.message_id.clone(), second.message_id.clone()];
            message_ids.sort();
            both_seen_tx.send(message_ids)?;
            let request_a = "plugin-hover-concurrent-a";
            let request_b = "plugin-hover-concurrent-b";
            let reply_a = if first.message_id == request_a {
                first.message_id.clone()
            } else if second.message_id == request_a {
                second.message_id.clone()
            } else {
                return Err("missing concurrent request a".into());
            };
            let reply_b = if first.message_id == request_b {
                first.message_id.clone()
            } else if second.message_id == request_b {
                second.message_id.clone()
            } else {
                return Err("missing concurrent request b".into());
            };
            write_ppc_reply_result(&mut server_stream, reply_b, r#"{"success":true,"seq":2}"#)?;
            write_ppc_reply_result(&mut server_stream, reply_a, r#"{"success":true,"seq":1}"#)?;
            Ok(())
        });

        let first_table = Arc::clone(&table);
        let first = std::thread::spawn(move || -> Result<Value, TestError> {
            Ok(first_table.dispatch(&Request {
                op: "plugin.lsp.hover".to_owned(),
                invocation_id: "plugin-hover-concurrent-a".to_owned(),
                args: json!({"agent_id": "agent-plugin", "request": "a"}),
            }))
        });
        let second_table = Arc::clone(&table);
        let second = std::thread::spawn(move || -> Result<Value, TestError> {
            Ok(second_table.dispatch(&Request {
                op: "plugin.lsp.hover".to_owned(),
                invocation_id: "plugin-hover-concurrent-b".to_owned(),
                args: json!({"agent_id": "agent-plugin", "request": "b"}),
            }))
        });

        let seen = both_seen_rx.recv_timeout(Duration::from_secs(1))?;
        assert_eq!(
            seen,
            vec![
                "plugin-hover-concurrent-a".to_owned(),
                "plugin-hover-concurrent-b".to_owned()
            ]
        );
        let first_response = join_value_thread(first, "first dispatch thread panicked")?;
        let second_response = join_value_thread(second, "second dispatch thread panicked")?;
        assert_eq!(first_response["success"], true);
        assert_eq!(first_response["seq"], 1);
        assert_eq!(second_response["success"], true);
        assert_eq!(second_response["seq"], 2);
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn read_only_ppc_failure_drops_connected_route() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("read-only-broken-ppc")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_manifest("digest-a", "hover"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        drop(server_stream);

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-broken-ppc".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["error"]["kind"], "internal_error");

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-broken-ppc".to_owned(),
            args: json!({}),
        });
        assert_eq!(status["connected_ppc_routes"], json!([]));
        assert_eq!(status["connected_ppc_services"], json!([]));
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn read_only_service_recovers_on_next_dispatch_after_ppc_failure() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let socket_root = test_socket_root("recover-after-ppc-failure");
        let (layer_stack_root, workspace_root) = test_bound_workspace("recover-after-ppc-failure")?;
        let command = vec![
            "/bin/sh",
            "-c",
            "test \"$EOS_PLUGIN_SERVICE_ID\" = pyright && sleep 30",
        ];
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-recover-after-ppc-failure".to_owned(),
            args: json!({
                "manifest": lsp_manifest_with_command("digest-a", "hover", command),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned(),
                "ppc_socket_root": socket_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.hover", client_stream)?;
        attach_service_snapshot_for_tests("plugin.lsp.hover")?;
        drop(server_stream);

        let failed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-broken-before-recovery".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(failed["error"]["kind"], "internal_error");

        let after_failure = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-recoverable-failure".to_owned(),
            args: json!({}),
        });
        assert_eq!(after_failure["connected_ppc_routes"], json!([]));
        assert_eq!(
            after_failure["loaded_plugins"][0]["services"][0]["state"],
            "stopped"
        );

        let connector = spawn_replying_connector(
            socket_root.clone(),
            r#"{"success":true,"from_recovered_service":true}"#,
        );
        let recovered = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-after-recovery".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(
            recovered["success"], true,
            "recovered response: {recovered:?}"
        );
        assert_eq!(recovered["from_recovered_service"], true);

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-recovery".to_owned(),
            args: json!({}),
        });
        let service = &status["loaded_plugins"][0]["services"][0];
        assert_eq!(service["state"], "ready");
        assert_eq!(service["restart_count"], 1);
        assert_eq!(status["connected_ppc_routes"], json!(["plugin.lsp.hover"]));

        join_test_thread(connector, "connector thread panicked")?;
        let _ = std::fs::remove_dir_all(socket_root);
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn connected_self_managed_plugin_op_services_occ_callback() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let layer_stack_root = test_layer_stack_root("self-managed-callback")?;
        let table = OpTable::with_builtins();
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_self_managed_manifest("digest-a", "apply"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": "/eos/plugin/workspace"
            }),
        });
        assert_eq!(ensure["success"], true);
        assert_eq!(
            ensure["operation_routes"][0]["dispatch_mode"],
            "self_managed_callback"
        );

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.apply", client_stream)?;
        let callback_root = layer_stack_root.clone();
        let server = std::thread::spawn(move || -> TestResult {
            let request = read_ppc_request(&mut server_stream, "read ppc request")?;
            assert_eq!(request.message_id, "plugin-apply-test");
            assert_eq!(request.op, "plugin.lsp.apply");

            let callback = PpcEnvelope {
                message_id: "plugin-apply-callback".to_owned(),
                direction: PpcDirection::Request,
                op: occ_callbacks::OCC_APPLY_CHANGESET_OP.to_owned(),
                body: serde_json::to_string(&json!({
                    "layer_stack_root": callback_root.to_string_lossy().into_owned(),
                    "changes": [{
                        "kind": "write",
                        "path": "src/main.py",
                        "content_utf8": "print('from callback')\n"
                    }]
                }))?,
            };
            server_stream.write_all(&callback.encode()?)?;
            let callback_reply = read_ppc_request(&mut server_stream, "read callback reply")?;
            assert_eq!(callback_reply.message_id, "plugin-apply-callback");
            let callback_body: Value = serde_json::from_str(&callback_reply.body)?;
            assert_eq!(callback_body["success"], true);
            assert_eq!(callback_body["files"][0]["status"], "committed");

            write_ppc_reply_result(
                &mut server_stream,
                request.message_id,
                r#"{"success":true,"from_self_managed":true}"#,
            )?;
            Ok(())
        });

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.apply".to_owned(),
            invocation_id: "plugin-apply-test".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], true, "routed response: {routed:?}");
        assert_eq!(routed["from_self_managed"], true);
        assert_eq!(
            read_layer_text(&layer_stack_root, "src/main.py")?,
            "print('from callback')\n"
        );

        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn self_managed_service_refreshes_after_peer_publish_before_request() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let table = OpTable::with_builtins();
        let (layer_stack_root, workspace_root) = test_bound_workspace("self-managed-refresh")?;
        let ensure = table.dispatch(&Request {
            op: "api.plugin.ensure".to_owned(),
            invocation_id: "plugin-ensure-test".to_owned(),
            args: json!({
                "manifest": lsp_self_managed_manifest("digest-a", "apply"),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned()
            }),
        });
        assert_eq!(ensure["success"], true);

        let (client_stream, mut server_stream) = ppc_stream_pair()?;
        register_ppc_client_for_tests("plugin.lsp.apply", client_stream)?;

        let write = table.dispatch(&Request {
            op: "api.v1.write_file".to_owned(),
            invocation_id: "peer-write".to_owned(),
            args: json!({
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "path": workspace_root.join("peer.txt").to_string_lossy().into_owned(),
                "content": "peer\n"
            }),
        });
        assert_eq!(write["success"], true, "write response: {write:?}");

        let server = std::thread::spawn(move || -> TestResult {
            let mut refresh_types = Vec::new();
            let mut current_manifest_key = String::new();
            loop {
                let request = read_ppc_request(&mut server_stream, "read ppc request")?;
                if request.op == WORKSPACE_SNAPSHOT_REFRESH_OP {
                    let body: Value = serde_json::from_str(&request.body)?;
                    refresh_types.push(
                        value_str(&body["type"], "refresh type must be a string")?.to_owned(),
                    );
                    if let Some(key) = body
                        .get("target_manifest_key")
                        .or_else(|| body.get("manifest_key"))
                        .and_then(Value::as_str)
                    {
                        current_manifest_key = key.to_owned();
                    }
                    let refresh_reply = json!({
                        "manifest_key": current_manifest_key,
                        "accepted": true
                    });
                    write_ppc_reply_json_result(
                        &mut server_stream,
                        request.message_id,
                        &refresh_reply,
                    )?;
                    continue;
                }

                assert_eq!(request.message_id, "plugin-apply-after-peer-write");
                assert_eq!(request.op, "plugin.lsp.apply");
                assert!(refresh_types.contains(&"prepare_refresh".to_owned()));
                assert!(refresh_types.contains(&"swap_workspace".to_owned()));
                assert!(refresh_types.contains(&"health".to_owned()));
                write_ppc_reply_result(
                    &mut server_stream,
                    request.message_id,
                    r#"{"success":true,"self_managed_after_refresh":true}"#,
                )?;
                break Ok(());
            }
        });

        let routed = table.dispatch(&Request {
            op: "plugin.lsp.apply".to_owned(),
            invocation_id: "plugin-apply-after-peer-write".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], true, "routed response: {routed:?}");
        assert_eq!(routed["self_managed_after_refresh"], true);

        let status = table.dispatch(&Request {
            op: "api.plugin.status".to_owned(),
            invocation_id: "plugin-status-after-self-managed-refresh".to_owned(),
            args: json!({}),
        });
        assert_eq!(status["loaded_plugins"][0]["services"][0]["state"], "ready");
        assert_eq!(
            status["loaded_plugins"][0]["services"][0]["refresh_count"],
            1
        );
        join_test_thread(server, "server thread panicked")?;
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    #[test]
    fn ensure_can_start_and_status_reports_service_process() -> TestResult {
        let _guard = PluginTestGuard::new()?;
        let socket_root = test_socket_root("ensure-start");
        let (layer_stack_root, workspace_root) = test_bound_workspace("ensure-start")?;
        let connector = spawn_replying_connector(
            socket_root.clone(),
            r#"{"success":true,"from_started_service":true}"#,
        );
        let command = vec![
            "/bin/sh",
            "-c",
            "test \"$EOS_PLUGIN_SERVICE_ID\" = pyright && sleep 30",
        ];
        let response = op_ensure(
            &json!({
                "manifest": lsp_manifest_with_command("digest-a", "hover", command),
                "layer_stack_root": layer_stack_root.to_string_lossy().into_owned(),
                "workspace_root": workspace_root.to_string_lossy().into_owned(),
                "ppc_socket_root": socket_root.to_string_lossy().into_owned(),
                "start_services": true
            }),
            DispatchContext::empty(),
        )?;

        assert_eq!(response["success"], true);
        assert_eq!(response["service_processes_started"], true);
        assert_eq!(
            response["running_service_processes"][0]["service_id"],
            "pyright"
        );
        assert_eq!(response["running_service_processes"][0]["running"], true);

        let status = op_status(&json!({}), DispatchContext::empty())?;
        assert_eq!(
            status["running_service_processes"][0]["service_id"],
            "pyright"
        );
        assert_eq!(status["running_service_processes"][0]["running"], true);

        let table = OpTable::with_builtins();
        let routed = table.dispatch(&Request {
            op: "plugin.lsp.hover".to_owned(),
            invocation_id: "plugin-hover-started-service".to_owned(),
            args: json!({"agent_id": "agent-plugin"}),
        });
        assert_eq!(routed["success"], true, "routed response: {routed:?}");
        assert_eq!(routed["from_started_service"], true);

        join_test_thread(connector, "connector thread panicked")?;
        let _ = std::fs::remove_dir_all(socket_root);
        remove_test_tree(&layer_stack_root)?;
        Ok(())
    }

    fn test_socket_root(name: &str) -> PathBuf {
        let root = PathBuf::from("target").join(format!("ppc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn test_layer_stack_root(name: &str) -> Result<PathBuf, TestError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "eos-plugin-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn test_bound_workspace(name: &str) -> Result<(PathBuf, PathBuf), TestError> {
        let layer_stack_root = test_layer_stack_root(name)?;
        let base = some_value(layer_stack_root.parent(), "layer root must have a parent")?;
        let workspace_root = base.join("workspace");
        std::fs::create_dir_all(&workspace_root)?;
        std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
        eos_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, true)?;
        Ok((layer_stack_root, workspace_root))
    }

    fn attach_service_snapshot_for_tests(op: &str) -> Result<(String, String), TestError> {
        let route = some_value(route_for_op(op)?, "registered plugin route missing")?;
        let service_key = some_value(route.service_key, "service key missing")?;
        let service_instance_id =
            some_value(route.service_instance_id, "service instance id missing")?;
        let snapshot = acquire_service_snapshot(&service_key, "test-health")?;
        let manifest_key = snapshot.manifest_key.clone();
        let old_snapshot = {
            let mut state = lock_state()?;
            mark_service_ready(&mut state, &service_instance_id, &snapshot, false)?;
            state
                .service_snapshots
                .insert(service_instance_id.clone(), snapshot)
        };
        if let Some(old_snapshot) = old_snapshot {
            release_service_snapshot(&old_snapshot);
        }
        Ok((service_instance_id, manifest_key))
    }

    fn remove_test_tree(layer_stack_root: &Path) -> TestResult {
        let base = some_value(
            layer_stack_root.parent(),
            "test layer root must have a parent",
        )?;
        let _ = std::fs::remove_dir_all(base);
        Ok(())
    }

    fn read_layer_text(root: &Path, path: &str) -> Result<String, TestError> {
        Ok(eos_layerstack::LayerStack::open(root.to_path_buf())?
            .read_text(path)?
            .0)
    }

    fn spawn_replying_connector(
        socket_root: PathBuf,
        reply_body: &'static str,
    ) -> std::thread::JoinHandle<TestResult> {
        std::thread::spawn(move || -> TestResult {
            let socket = wait_for_socket(&socket_root)?;
            let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
            let request = read_ppc_request(&mut stream, "read ppc request")?;
            write_ppc_reply_result(&mut stream, request.message_id, reply_body)?;
            Ok(())
        })
    }

    fn spawn_restart_connector(
        socket_root: PathBuf,
        allow_reconnect_rx: mpsc::Receiver<()>,
        reply_body: &'static str,
    ) -> std::thread::JoinHandle<TestResult> {
        std::thread::spawn(move || -> TestResult {
            let _old_stream = connect_ppc_socket(&socket_root)?;
            allow_reconnect_rx.recv()?;

            let mut stream = connect_ppc_socket(&socket_root)?;
            let request = read_ppc_request(&mut stream, "read restarted ppc request")?;
            assert_eq!(request.op, "plugin.lsp.hover");
            write_ppc_reply_result(&mut stream, request.message_id, reply_body)?;
            Ok(())
        })
    }

    fn wait_for_socket(root: &Path) -> Result<PathBuf, std::io::Error> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) == Some("sock") {
                        return Ok(path);
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("timed out waiting for socket under {}", root.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn connect_ppc_socket(root: &Path) -> Result<std::os::unix::net::UnixStream, std::io::Error> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("sock") {
                        continue;
                    }
                    if let Ok(stream) = std::os::unix::net::UnixStream::connect(path) {
                        return Ok(stream);
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("timed out connecting to socket under {}", root.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

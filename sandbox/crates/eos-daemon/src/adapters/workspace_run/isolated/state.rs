//! Daemon-local isolated-workspace session state.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use eos_config::configs::isolated_workspace::{
    IsolatedWorkspaceConfig, Rfc1918Egress as ConfigRfc1918Egress,
};
use eos_layerstack::{read_workspace_binding, LayerStack};
use eos_workspace_modes::isolated::{
    IsolatedError, IsolatedSession, JsonlAuditSink, ResourceCaps,
    Rfc1918Egress as RuntimeRfc1918Egress,
};

use super::errors::setup_error;
use super::runtime::{DaemonLayerStackPort, DaemonNamespaceRuntime};

type DaemonSession = IsolatedSession<DaemonLayerStackPort, DaemonNamespaceRuntime, JsonlAuditSink>;

pub(super) struct DaemonIsolatedState {
    #[cfg(target_os = "linux")]
    pub(super) layer_stack_root: PathBuf,
    pub(super) session: DaemonSession,
}

pub(crate) fn configure_isolated_workspace(config: &IsolatedWorkspaceConfig) {
    let mut guard = isolated_workspace_config_cell()
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    *guard = config.clone();
}

pub(super) fn ensure_state(root: &Path) -> Result<(), IsolatedError> {
    let root = normalized_root(root);
    {
        let mut guard = lock_state_cell();
        #[cfg(target_os = "linux")]
        if let Some(state) = guard.as_mut() {
            if state.layer_stack_root != root {
                // Block rebinding to a new root only while an isolated workspace
                // is open: those handles pin leases/namespaces on the old root.
                // (Isolated command sessions belong to an open caller, so this
                // already covers them; ephemeral command sessions are unrelated
                // to the isolated manager's binding and must not block a rebind.)
                let open_callers = state.session.list_open_callers();
                if !open_callers.is_empty() {
                    return Err(IsolatedError::SetupFailed {
                        step: format!(
                            "isolated workspace manager is bound to {} with active callers",
                            state.layer_stack_root.display()
                        ),
                    });
                }
                state.session.reap_orphan_resources();
                *guard = None;
            }
        }
        if guard.is_none() {
            let config = isolated_workspace_config();
            let mut caps = resource_caps_from_config(&config);
            if !caps.enabled {
                return Err(IsolatedError::FeatureDisabled);
            }
            if let Some(binding) = read_workspace_binding(&root).map_err(setup_error)? {
                caps.eos_workspace_root = binding.workspace_root;
            }
            let stack = LayerStack::open(root.clone()).map_err(setup_error)?;
            let mut session = IsolatedSession::with_scratch_root(
                caps,
                DaemonLayerStackPort {
                    stack: Arc::new(Mutex::new(stack)),
                },
                DaemonNamespaceRuntime,
                JsonlAuditSink::new(&config.audit_jsonl_path),
                config.scratch_root,
            );
            session.initialize()?;
            *guard = Some(DaemonIsolatedState {
                #[cfg(target_os = "linux")]
                layer_stack_root: root,
                session,
            });
        }
    }
    Ok(())
}

fn normalized_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

pub(super) fn isolated_workspace_config() -> IsolatedWorkspaceConfig {
    isolated_workspace_config_cell()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn isolated_workspace_config_cell() -> &'static std::sync::RwLock<IsolatedWorkspaceConfig> {
    static CONFIG: OnceLock<std::sync::RwLock<IsolatedWorkspaceConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| std::sync::RwLock::new(default_isolated_workspace_config()))
}

pub(super) fn default_isolated_workspace_config() -> IsolatedWorkspaceConfig {
    IsolatedWorkspaceConfig {
        enabled: false,
        scratch_root: PathBuf::from("/eos/scratch/isolated"),
        audit_jsonl_path: PathBuf::from("/eos/scratch/isolated/audit.jsonl"),
        ttl_s: 1800.0,
        total_cap: 5,
        upperdir_bytes: 1_073_741_824,
        memavail_fraction: 0.5,
        setup_timeout_s: 30.0,
        exit_grace_s: 0.25,
        rfc1918_egress: ConfigRfc1918Egress::Allow,
        fallback_dns: "1.1.1.1".to_owned(),
        workspace_root: PathBuf::from("/testbed"),
        sample_interval_s: 0.5,
    }
}

fn resource_caps_from_config(config: &IsolatedWorkspaceConfig) -> ResourceCaps {
    ResourceCaps {
        enabled: config.enabled,
        ttl_s: config.ttl_s,
        total_cap: config.total_cap,
        upperdir_bytes: config.upperdir_bytes,
        memavail_fraction: config.memavail_fraction,
        setup_timeout_s: config.setup_timeout_s,
        exit_grace_s: config.exit_grace_s,
        rfc1918_egress: match config.rfc1918_egress {
            ConfigRfc1918Egress::Allow => RuntimeRfc1918Egress::Allow,
            ConfigRfc1918Egress::Deny => RuntimeRfc1918Egress::Deny,
        },
        fallback_dns: config.fallback_dns.clone(),
        eos_workspace_root: config.workspace_root.to_string_lossy().into_owned(),
        sample_interval_s: config.sample_interval_s,
    }
}

pub(super) fn with_state<T>(
    f: impl FnOnce(&mut DaemonIsolatedState) -> Result<T, IsolatedError>,
) -> Result<T, IsolatedError> {
    lock_state_cell()
        .as_mut()
        .ok_or(IsolatedError::FeatureDisabled)
        .and_then(f)
}

fn state_cell() -> &'static Mutex<Option<DaemonIsolatedState>> {
    static STATE: OnceLock<Mutex<Option<DaemonIsolatedState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

pub(super) fn lock_state_cell() -> MutexGuard<'static, Option<DaemonIsolatedState>> {
    state_cell().lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn reset_test_manager_file() {
    let session_root = isolated_workspace_config().scratch_root;
    let _ = std::fs::remove_dir_all(&session_root);
    if std::fs::create_dir_all(&session_root).is_err() {
        return;
    }
    let _ = std::fs::write(
        session_root.join("manager.json"),
        br#"{"schema_version":1,"handles":[]}"#,
    );
}

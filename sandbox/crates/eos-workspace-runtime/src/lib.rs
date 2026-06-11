//! Isolated-workspace runtime: lease custody, session lifecycle, TTL sweep
//! policy, and the caller-keyed workspace-run cancel coordinator.
//!
//! The daemon composes this service: it parses wire args, gates cross-domain
//! policy, calls one [`WorkspaceRuntime`] method, and shapes one response.
//! This crate owns the policy half — when leases are acquired and released,
//! when sessions are torn down, and in what order — while namespace mechanics
//! stay in `eos-isolated-workspace` and command-session internals stay in
//! `eos-command-ops`. State lives on a [`WorkspaceRuntime`] instance, never in
//! process globals.
//!
//! Lock-order discipline: the workspace state lock is acquired before any
//! command-session registry call, and the manager's own exit path runs under
//! the state lock exactly as the pre-extraction daemon implementation did.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use eos_command_ops::CommandBinding;
use eos_config::configs::isolated_workspace::{
    IsolatedWorkspaceConfig, Rfc1918Egress as ConfigRfc1918Egress,
};
use eos_isolated_workspace::{
    IsolatedError, IsolatedManager, IsolatedSnapshot, ResourceCaps,
    Rfc1918Egress as RuntimeRfc1918Egress, WorkspaceHandle,
};
use eos_layerstack::{read_workspace_binding, LayerStack};

fn setup_error(error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::SetupFailed {
        step: error.to_string(),
    }
}

struct BoundState {
    layer_stack_root: PathBuf,
    stack: LayerStack,
    manager: IsolatedManager,
}

impl BoundState {
    /// Acquire a snapshot lease for `caller_id` and shape it for `enter`.
    fn acquire_snapshot(&self, caller_id: &str) -> Result<IsolatedSnapshot, IsolatedError> {
        let lease = self
            .stack
            .acquire_snapshot(&format!("isolated-{caller_id}"))
            .map_err(setup_error)?;
        Ok(IsolatedSnapshot {
            lease_id: lease.lease_id,
            manifest_version: lease.manifest_version,
            manifest_root_hash: lease.root_hash,
            layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
        })
    }

    /// Best-effort lease release; returns whether the lease was held.
    fn release_lease(&mut self, lease_id: &str) -> Option<bool> {
        self.stack.release_lease(lease_id).ok()
    }

    /// Exit `caller_id`'s workspace and release its lease, shaping the typed
    /// outcome with the lease custody fields.
    fn exit_caller(
        &mut self,
        caller_id: &str,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedError> {
        let isolated = self.manager.exit(caller_id, grace_s)?;
        let lease_released = self.release_lease(&isolated.lease_id);
        let active_leases_after = self.stack.active_lease_count();
        Ok(ExitOutcome {
            isolated,
            lease_released,
            active_leases_after,
        })
    }
}

/// Typed result of one isolated-workspace exit: the manager's teardown outcome
/// plus the lease custody fields the daemon adapter splices into the wire
/// inspection object.
pub struct ExitOutcome {
    /// The namespace/cgroup/scratch teardown outcome from the isolated manager.
    pub isolated: eos_isolated_workspace::ExitOutcome,
    /// Whether the workspace's snapshot lease was still held at release.
    pub lease_released: Option<bool>,
    /// Active leases remaining on the bound stack after release.
    pub active_leases_after: usize,
}

/// Outcome of tearing down one caller's workspace runs.
pub struct CallerCancel {
    /// Command sessions that were live at entry (now cancelled + discarded).
    pub cancelled_sessions: usize,
    /// Isolated-workspace teardown result: the typed exit outcome if the
    /// caller was isolated, `Err(IsolatedError::NotOpen)` if it was ephemeral
    /// (or had no isolated workspace), or another `IsolatedError` on teardown
    /// failure.
    pub isolated: Result<ExitOutcome, IsolatedError>,
}

/// Instance-owned isolated-workspace service state: the typed config plus the
/// lazily bound layer-stack + manager pair.
pub struct WorkspaceRuntime {
    config: IsolatedWorkspaceConfig,
    state: Mutex<Option<BoundState>>,
}

impl WorkspaceRuntime {
    #[must_use]
    pub fn new(config: IsolatedWorkspaceConfig) -> Self {
        Self {
            config,
            state: Mutex::new(None),
        }
    }

    /// Open an isolated workspace for `caller_id` on the stack at `root`:
    /// bind (or rebind) the manager, acquire a snapshot lease, and enter. The
    /// lease is released again when `enter` fails.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::FeatureDisabled`] when isolation is disabled,
    /// and the manager's enter/setup errors otherwise.
    pub fn enter(&self, caller_id: &str, root: &Path) -> Result<WorkspaceHandle, IsolatedError> {
        self.ensure_state(root)?;
        self.with_state(|state| {
            let snapshot = state.acquire_snapshot(caller_id)?;
            let lease_id = snapshot.lease_id.clone();
            match state.manager.enter(caller_id, snapshot) {
                Ok(handle) => Ok(handle),
                Err(error) => {
                    let _ = state.release_lease(&lease_id);
                    Err(error)
                }
            }
        })
    }

    /// Tear down `caller_id`'s isolated workspace if open: namespace/network/
    /// cgroup, release the lease, discard the upperdir (never published). The
    /// single isolated-teardown primitive shared by the exit op and the
    /// workspace-run cancel surface.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NotOpen`] when the caller is not isolated (the
    /// cancel surface treats that as a no-op), and teardown errors otherwise.
    pub fn exit(&self, caller_id: &str, grace_s: Option<f64>) -> Result<ExitOutcome, IsolatedError> {
        self.with_state(|state| state.exit_caller(caller_id, grace_s))
    }

    /// The caller's open handle, or `Ok(None)` when no workspace is open.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::FeatureDisabled`] when isolation is disabled.
    pub fn status(&self, caller_id: &str) -> Result<Option<WorkspaceHandle>, IsolatedError> {
        self.with_state(|state| Ok(state.manager.get_handle(caller_id)))
    }

    /// Caller ids with an open isolated workspace (empty when disabled).
    #[must_use]
    pub fn list_open(&self) -> Vec<String> {
        self.lock_state_cell()
            .as_ref()
            .map(|state| state.manager.list_open_callers())
            .unwrap_or_default()
    }

    /// Bump the caller's isolated-workspace TTL liveness (file/command
    /// activity).
    pub fn touch(&self, caller_id: &str) {
        let mut guard = self.lock_state_cell();
        if let Some(state) = guard.as_mut() {
            state.manager.touch(caller_id);
        }
    }

    /// Whether `caller_id` currently owns an open isolated workspace.
    #[must_use]
    pub fn caller_has_active_handle(&self, caller_id: &str) -> bool {
        let caller_id = caller_id.trim();
        if caller_id.is_empty() {
            return false;
        }
        let guard = self.lock_state_cell();
        guard
            .as_ref()
            .and_then(|state| state.manager.get_handle(caller_id))
            .is_some()
    }

    /// The command-session binding for `caller_id`'s open workspace, or `None`
    /// when the caller is not isolated (callers then route ephemerally).
    #[must_use]
    pub fn command_binding_for(&self, caller_id: &str) -> Option<CommandBinding> {
        if caller_id.is_empty() {
            return None;
        }
        let guard = self.lock_state_cell();
        let state = guard.as_ref()?;
        let handle = state.manager.get_handle(caller_id)?;
        Some(command_binding_from(&state.layer_stack_root, handle))
    }

    /// Cancel every workspace run owned by `caller_id`: discard its command
    /// session(s), then exit its isolated workspace if open. The order matters
    /// — sessions are cancelled before the isolated namespace/lease teardown.
    pub fn cancel_runs_for_caller(&self, caller_id: &str, grace_s: Option<f64>) -> CallerCancel {
        let cancelled_sessions =
            eos_command_ops::cleanup_command_sessions_for_caller(caller_id, grace_s);
        let isolated = self.exit(caller_id, grace_s);
        CallerCancel {
            cancelled_sessions,
            isolated,
        }
    }

    /// Cancel every workspace run in the sandbox: discard all command
    /// sessions, exit every isolated caller, then reap orphaned namespace/
    /// cgroup/scratch resources. Returns the per-substrate counts as
    /// `(cancelled_sessions, isolated_callers_exited)`.
    pub fn cancel_all_runs(&self, grace_s: Option<f64>) -> (usize, usize) {
        let cancelled_sessions = eos_command_ops::cancel_all_command_sessions(grace_s);
        let isolated_exited = self.exit_all_and_reap(grace_s);
        (cancelled_sessions, isolated_exited)
    }

    /// Exit every open isolated workspace and reap orphaned resources (the
    /// whole-sandbox cancel sweep). Returns the number of callers exited.
    fn exit_all_and_reap(&self, grace_s: Option<f64>) -> usize {
        let mut guard = self.lock_state_cell();
        let Some(state) = guard.as_mut() else {
            return 0;
        };
        let callers = state.manager.list_open_callers();
        for caller in &callers {
            let _ = state.exit_caller(caller, grace_s);
        }
        state.manager.reap_orphan_resources();
        callers.len()
    }

    /// Evict idle isolated workspaces past their TTL, releasing their leases.
    /// Callers that still own a live command session are protected.
    pub fn ttl_sweep(&self) -> usize {
        let mut guard = self.lock_state_cell();
        let Some(state) = guard.as_mut() else {
            return 0;
        };
        // The command-session registry is the authority for caller liveness
        // (lock order: workspace state -> command-session registry).
        let active_callers = state
            .manager
            .list_open_callers()
            .into_iter()
            .filter(|caller| eos_command_ops::active_command_sessions_for_caller(caller) > 0)
            .collect::<HashSet<_>>();
        let evicted = state.manager.ttl_sweep(&active_callers);
        let count = evicted.len();
        for outcome in evicted {
            let _ = state.release_lease(&outcome.lease_id);
        }
        count
    }

    /// Exit every caller, drop the bound state, and rewrite the persisted
    /// manager file (backs `sandbox.isolation.test_reset`). Returns the caller
    /// ids that were exited.
    pub fn test_reset(&self) -> Vec<String> {
        let exited_callers = {
            let mut guard = self.lock_state_cell();
            let exited_callers = if let Some(state) = guard.as_mut() {
                let callers = state.manager.list_open_callers();
                for caller_id in &callers {
                    let _ = state.exit_caller(caller_id, Some(0.0));
                }
                state.manager.reap_orphan_resources();
                callers
            } else {
                Vec::new()
            };
            *guard = None;
            exited_callers
        };
        self.reset_test_manager_file();
        exited_callers
    }

    /// Bind (or rebind) the isolated manager to `root`, initializing caps from
    /// the runtime config and releasing leases orphaned by a prior daemon.
    fn ensure_state(&self, root: &Path) -> Result<(), IsolatedError> {
        let root = normalized_root(root);
        {
            let mut guard = self.lock_state_cell();
            if let Some(state) = guard.as_mut() {
                if state.layer_stack_root != root {
                    // Block rebinding to a new root only while an isolated workspace
                    // is open: those handles pin leases/namespaces on the old root.
                    // (Isolated command sessions belong to an open caller, so this
                    // already covers them; ephemeral command sessions are unrelated
                    // to the isolated manager's binding and must not block a rebind.)
                    let open_callers = state.manager.list_open_callers();
                    if !open_callers.is_empty() {
                        return Err(IsolatedError::SetupFailed {
                            step: format!(
                                "isolated workspace manager is bound to {} with active callers",
                                state.layer_stack_root.display()
                            ),
                        });
                    }
                    state.manager.reap_orphan_resources();
                    *guard = None;
                }
            }
            if guard.is_none() {
                let mut caps = resource_caps_from_config(&self.config);
                if !caps.enabled {
                    return Err(IsolatedError::FeatureDisabled);
                }
                if let Some(binding) = read_workspace_binding(&root).map_err(setup_error)? {
                    caps.eos_workspace_root = binding.workspace_root;
                }
                let mut stack = LayerStack::open(root.clone()).map_err(setup_error)?;
                let mut manager =
                    IsolatedManager::with_scratch_root(caps, self.config.scratch_root.clone());
                let orphan_lease_ids = manager.initialize()?;
                for lease_id in orphan_lease_ids {
                    let _ = stack.release_lease(&lease_id);
                }
                *guard = Some(BoundState {
                    layer_stack_root: root,
                    stack,
                    manager,
                });
            }
        }
        Ok(())
    }

    fn with_state<T>(
        &self,
        f: impl FnOnce(&mut BoundState) -> Result<T, IsolatedError>,
    ) -> Result<T, IsolatedError> {
        self.lock_state_cell()
            .as_mut()
            .ok_or(IsolatedError::FeatureDisabled)
            .and_then(f)
    }

    fn lock_state_cell(&self) -> MutexGuard<'_, Option<BoundState>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn reset_test_manager_file(&self) {
        let session_root = &self.config.scratch_root;
        let _ = std::fs::remove_dir_all(session_root);
        if std::fs::create_dir_all(session_root).is_err() {
            return;
        }
        let _ = std::fs::write(
            session_root.join("manager.json"),
            br#"{"schema_version":1,"handles":[]}"#,
        );
    }
}

fn command_binding_from(layer_stack_root: &Path, handle: WorkspaceHandle) -> CommandBinding {
    CommandBinding {
        caller_id: handle.caller_id,
        workspace_handle_id: handle.workspace_id.0,
        layer_stack_root: layer_stack_root.to_path_buf(),
        manifest_version: handle.manifest_version,
        manifest_root_hash: handle.manifest_root_hash,
        workspace_root: PathBuf::from(handle.workspace_root),
        scratch_dir: handle.scratch_dir,
        upperdir: handle.upperdir,
        workdir: handle.workdir,
        layer_paths: handle.layer_paths,
        ns_fds: handle.ns_fds,
        cgroup_path: handle.cgroup_path,
    }
}

fn normalized_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
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
    }
}

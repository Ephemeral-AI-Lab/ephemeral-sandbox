use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

use sandbox_observability_telemetry::Observer;
use sandbox_runtime_namespace_execution::NamespaceExecutionId;

use crate::layerstack::LayerStackService;
use crate::namespace_execution::{WorkspaceCommandReleaseProof, WorkspaceCommandTeardown};
use crate::services::WorkloadCgroupLimits;
use crate::workspace_crate::{DestroyWorkspaceResult, WorkspaceRuntimeService, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionError;

use super::cgroup::cleanup_workspace_cgroup;
use super::model::{
    FinalizePolicy, HolderExitDisposition, HolderLifecycleEvent, HolderLifecycleEventKind,
    HolderLifecycleSnapshot, WorkspaceSession, WorkspaceSessionHandler,
};

const HOLDER_LIFECYCLE_EVENT_CAPACITY: usize = 128;
const HOLDER_LIFECYCLE_DETAIL_CAPACITY: usize = 512;

pub(crate) type DestroyFlightResult = Result<DestroyWorkspaceResult, WorkspaceSessionError>;

#[derive(Clone)]
pub(crate) struct HolderDestroyPlan {
    pub(crate) handler: WorkspaceSessionHandler,
    pub(crate) policy: FinalizePolicy,
    pub(crate) command_ids: Vec<NamespaceExecutionId>,
    pub(crate) reason: String,
    pub(crate) newly_observed: bool,
    pub(crate) attempt: u8,
}

#[derive(Clone)]
pub(crate) struct DestroyFlightTerminal {
    pub(crate) result: DestroyFlightResult,
    pub(crate) holder_disposition: Option<HolderExitDisposition>,
}

pub(crate) struct DestroyFlight {
    pub(crate) handler: WorkspaceSessionHandler,
    pub(crate) holder_plan: Option<HolderDestroyPlan>,
    pub(crate) terminal: Mutex<Option<DestroyFlightTerminal>>,
    pub(crate) ready: Condvar,
    pub(crate) waiters: AtomicUsize,
}

pub(crate) struct CreateReservation<'a> {
    service: &'a WorkspaceSessionService,
    workspace_session_id: WorkspaceSessionId,
}

impl Drop for CreateReservation<'_> {
    fn drop(&mut self) {
        self.service
            .creating_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.workspace_session_id);
    }
}

impl DestroyFlight {
    pub(crate) fn new(
        handler: WorkspaceSessionHandler,
        holder_plan: Option<HolderDestroyPlan>,
    ) -> Self {
        Self {
            handler,
            holder_plan,
            terminal: Mutex::new(None),
            ready: Condvar::new(),
            waiters: AtomicUsize::new(0),
        }
    }
}

pub(crate) struct HolderLifecycleLog {
    events: VecDeque<HolderLifecycleEvent>,
    next_sequence: u64,
    holder_exit_total: u64,
    cleanup_attempt_total: u64,
    cleanup_failure_total: u64,
    cleanup_terminal_total: u64,
    dropped_event_total: u64,
}

impl HolderLifecycleLog {
    fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(HOLDER_LIFECYCLE_EVENT_CAPACITY),
            next_sequence: 1,
            holder_exit_total: 0,
            cleanup_attempt_total: 0,
            cleanup_failure_total: 0,
            cleanup_terminal_total: 0,
            dropped_event_total: 0,
        }
    }

    pub(crate) fn record(
        &mut self,
        workspace_session_id: WorkspaceSessionId,
        kind: HolderLifecycleEventKind,
        detail: String,
        cleanup_duration_ms: Option<u64>,
    ) {
        match kind {
            HolderLifecycleEventKind::ExitObserved => {
                self.holder_exit_total = self.holder_exit_total.saturating_add(1);
            }
            HolderLifecycleEventKind::CleanupAttempt => {
                self.cleanup_attempt_total = self.cleanup_attempt_total.saturating_add(1);
            }
            HolderLifecycleEventKind::CleanupFailure => {
                self.cleanup_failure_total = self.cleanup_failure_total.saturating_add(1);
            }
            HolderLifecycleEventKind::CleanupTerminal => {
                self.cleanup_terminal_total = self.cleanup_terminal_total.saturating_add(1);
            }
        }
        if self.events.len() == HOLDER_LIFECYCLE_EVENT_CAPACITY {
            self.events.pop_front();
            self.dropped_event_total = self.dropped_event_total.saturating_add(1);
        }
        self.events.push_back(HolderLifecycleEvent {
            sequence: self.next_sequence,
            workspace_session_id,
            kind,
            detail: bounded_lifecycle_detail(detail),
            cleanup_duration_ms,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
    }

    pub(crate) fn snapshot(&self) -> HolderLifecycleSnapshot {
        HolderLifecycleSnapshot {
            holder_exit_total: self.holder_exit_total,
            cleanup_attempt_total: self.cleanup_attempt_total,
            cleanup_failure_total: self.cleanup_failure_total,
            cleanup_terminal_total: self.cleanup_terminal_total,
            dropped_event_total: self.dropped_event_total,
            events: self.events.iter().cloned().collect(),
        }
    }
}

fn bounded_lifecycle_detail(mut detail: String) -> String {
    if detail.len() <= HOLDER_LIFECYCLE_DETAIL_CAPACITY {
        return detail;
    }
    let mut end = HOLDER_LIFECYCLE_DETAIL_CAPACITY;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail.truncate(end);
    detail
}

pub struct WorkspaceSessionService {
    sessions: Mutex<HashMap<WorkspaceSessionId, WorkspaceSession>>,
    creating_sessions: Mutex<HashSet<WorkspaceSessionId>>,
    gates: Mutex<HashMap<WorkspaceSessionId, Arc<Mutex<()>>>>,
    pub(crate) destroy_flights: Mutex<HashMap<WorkspaceSessionId, Arc<DestroyFlight>>>,
    pub(crate) holder_lifecycle: Mutex<HolderLifecycleLog>,
    command_teardown: Mutex<Option<Weak<dyn WorkspaceCommandTeardown>>>,
    workspace: Arc<WorkspaceRuntimeService>,
    layerstack: Arc<LayerStackService>,
    cgroup_root: Option<PathBuf>,
    pub(super) workload_cgroup_limits: Option<WorkloadCgroupLimits>,
    pub(super) workload_cgroup_unavailable_reason: Option<String>,
    obs: Observer,
}

impl WorkspaceSessionService {
    pub(crate) const fn workload_cgroup_limits(&self) -> Option<WorkloadCgroupLimits> {
        self.workload_cgroup_limits
    }

    #[must_use]
    pub fn new(
        workspace: Arc<WorkspaceRuntimeService>,
        layerstack: Arc<LayerStackService>,
        obs: Observer,
    ) -> Self {
        Self::with_cgroup_root(workspace, layerstack, None, obs)
    }

    #[must_use]
    pub fn with_cgroup_root(
        workspace: Arc<WorkspaceRuntimeService>,
        layerstack: Arc<LayerStackService>,
        cgroup_root: Option<PathBuf>,
        obs: Observer,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::with_capacity(1)),
            creating_sessions: Mutex::new(HashSet::with_capacity(1)),
            gates: Mutex::new(HashMap::with_capacity(1)),
            destroy_flights: Mutex::new(HashMap::with_capacity(1)),
            holder_lifecycle: Mutex::new(HolderLifecycleLog::new()),
            command_teardown: Mutex::new(None),
            workspace,
            layerstack,
            cgroup_root,
            workload_cgroup_limits: None,
            workload_cgroup_unavailable_reason: Some(
                "workload cgroup limits are not configured".to_owned(),
            ),
            obs,
        }
    }

    #[must_use]
    pub fn with_workload_cgroup(
        workspace: Arc<WorkspaceRuntimeService>,
        layerstack: Arc<LayerStackService>,
        cgroup_root: PathBuf,
        limits: WorkloadCgroupLimits,
        obs: Observer,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::with_capacity(1)),
            creating_sessions: Mutex::new(HashSet::with_capacity(1)),
            gates: Mutex::new(HashMap::with_capacity(1)),
            destroy_flights: Mutex::new(HashMap::with_capacity(1)),
            holder_lifecycle: Mutex::new(HolderLifecycleLog::new()),
            command_teardown: Mutex::new(None),
            workspace,
            layerstack,
            cgroup_root: Some(cgroup_root),
            workload_cgroup_limits: Some(limits),
            workload_cgroup_unavailable_reason: None,
            obs,
        }
    }

    #[must_use]
    pub fn with_unavailable_workload_cgroup(
        workspace: Arc<WorkspaceRuntimeService>,
        layerstack: Arc<LayerStackService>,
        limits: WorkloadCgroupLimits,
        reason: String,
        obs: Observer,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::with_capacity(1)),
            creating_sessions: Mutex::new(HashSet::with_capacity(1)),
            gates: Mutex::new(HashMap::with_capacity(1)),
            destroy_flights: Mutex::new(HashMap::with_capacity(1)),
            holder_lifecycle: Mutex::new(HolderLifecycleLog::new()),
            command_teardown: Mutex::new(None),
            workspace,
            layerstack,
            cgroup_root: None,
            workload_cgroup_limits: Some(limits),
            workload_cgroup_unavailable_reason: Some(reason),
            obs,
        }
    }

    /// The per-session admission gate: the single serializer for command
    /// admission, completion, and finalization, session file ops, remounts,
    /// and guarded/faulty destroys. It does not serialize any public capture —
    /// capture exists only inside the finalize runner, which runs under the
    /// gate already held by the completing path. The gates map is locked only
    /// to clone or drop an Arc — never wait on a gate while holding a map
    /// (lock order: gate → sessions map → storage writer lock; the gates map
    /// may briefly take `sessions` inside [`Self::discard_resurrected_gate`],
    /// so nothing may take the gates map while holding `sessions`).
    pub(crate) fn session_gate(&self, workspace_id: &WorkspaceSessionId) -> Arc<Mutex<()>> {
        self.gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace_id.clone())
            .or_default()
            .clone()
    }

    pub(crate) fn drop_session_gate(&self, workspace_id: &WorkspaceSessionId) {
        self.gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(workspace_id);
    }

    /// Gates-map hygiene (§2.3): a gate-then-resolve path that failed
    /// `not_found` removes the gates-map entry it may have resurrected —
    /// only when the map entry is still the same `Arc` and the sessions map
    /// has no entry for the id, so a concurrently re-created session or a
    /// stuck `finalize_failed` session keeps its gate.
    pub(crate) fn discard_resurrected_gate(
        &self,
        workspace_id: &WorkspaceSessionId,
        gate: &Arc<Mutex<()>>,
    ) {
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        let same_entry = gates
            .get(workspace_id)
            .is_some_and(|entry| Arc::ptr_eq(entry, gate));
        if !same_entry {
            return;
        }
        let session_absent = self
            .sessions
            .lock()
            .map(|sessions| !sessions.contains_key(workspace_id))
            .unwrap_or(false);
        if session_absent {
            gates.remove(workspace_id);
        }
    }

    /// Number of live gates-map entries. Observability for the gates-map
    /// hygiene rule; not part of the operational API.
    #[doc(hidden)]
    #[must_use]
    pub fn gate_entry_count(&self) -> usize {
        self.gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn destroy_flight_waiter_count(&self, workspace_session_id: &WorkspaceSessionId) -> usize {
        self.destroy_flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace_session_id)
            .map_or(0, |flight| flight.waiters.load(Ordering::Acquire))
    }

    /// Snapshot of the live session ids for the post-commit remount sweep.
    #[must_use]
    pub fn session_ids(&self) -> Vec<WorkspaceSessionId> {
        let _ = self.reconcile_holder_exits();
        self.sessions
            .lock()
            .map(|sessions| {
                let mut ids: Vec<WorkspaceSessionId> = sessions.keys().cloned().collect();
                ids.sort_by(|left, right| left.0.cmp(&right.0));
                ids
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub(crate) fn workspace(&self) -> &Arc<WorkspaceRuntimeService> {
        &self.workspace
    }

    #[must_use]
    pub(crate) fn layerstack(&self) -> &Arc<LayerStackService> {
        &self.layerstack
    }

    /// Resolve a workspace session to its isolated-network IP.
    ///
    /// `Err(NotFound)` means no such session; `Ok(None)` means the session
    /// exists but has no reachable isolated IP (shared network or no veth);
    /// `Ok(Some(ip))` is the workspace IP a forwarder can dial.
    /// Resolution is serialized by the per-session admission gate and only an
    /// `Active` session is reachable. In particular, a post-commit
    /// `FinalizeFailed` session is cleanup-only and cannot receive forwarded
    /// traffic while guarded destroy remains available.
    ///
    /// # Errors
    /// Returns [`WorkspaceSessionError::NotFound`] for an unknown session, or a
    /// lock/runtime error when session or workspace state cannot be read.
    pub fn isolated_ip(
        &self,
        workspace_id: &WorkspaceSessionId,
    ) -> Result<Option<std::net::Ipv4Addr>, WorkspaceSessionError> {
        self.with_gated_session(workspace_id, |_| self.workspace.isolated_ip(workspace_id))?
            .map_err(WorkspaceSessionError::from)
    }

    #[must_use]
    pub(crate) fn obs(&self) -> &Observer {
        &self.obs
    }

    /// Create the leaf workspace cgroup `R/workspace-<wsid>` when a delegated
    /// cgroup root is configured. An unconfigured root is an explicit
    /// unsupported/degraded capability and yields `Ok(None)`. Once a root is
    /// configured, creation and every requested limit write are fail-closed;
    /// a partial leaf is rolled back and the raw workspace caller aborts.
    pub(crate) fn prepare_workspace_cgroup(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<Option<PathBuf>, WorkspaceSessionError> {
        let Some(path) = self.workspace_cgroup_path(workspace_session_id) else {
            return Ok(None);
        };
        std::fs::create_dir_all(&path).map_err(|error| {
            WorkspaceSessionError::WorkloadCgroupSetupFailed {
                workspace_session_id: workspace_session_id.clone(),
                diagnostic: format!("create {}: {error}", path.display()),
                rollback_diagnostic: None,
            }
        })?;
        if let Some(limits) = self.workload_cgroup_limits {
            if let Err(error) = write_workload_cgroup_limits(&path, limits) {
                let rollback_diagnostic = cleanup_workspace_cgroup(&path).err();
                return Err(WorkspaceSessionError::WorkloadCgroupSetupFailed {
                    workspace_session_id: workspace_session_id.clone(),
                    diagnostic: format!("configure {}: {error}", path.display()),
                    rollback_diagnostic,
                });
            }
        }
        Ok(Some(path))
    }

    pub(crate) fn workspace_cgroup_path(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Option<PathBuf> {
        self.cgroup_root
            .as_ref()
            .map(|root| root.join(format!("workspace-{}", workspace_session_id.0)))
    }

    pub(crate) fn lock_sessions(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<WorkspaceSessionId, WorkspaceSession>>, WorkspaceSessionError>
    {
        self.sessions
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)
    }

    /// Reserve an operation-layer identity before either the raw manager or
    /// workload-cgroup setup can mutate resources. The reservation and active
    /// map are checked under one lock order (`creating_sessions` then
    /// `sessions`), so concurrent creators cannot both cross the boundary.
    pub(crate) fn reserve_workspace_session_id(
        &self,
        workspace_session_id: WorkspaceSessionId,
    ) -> Result<CreateReservation<'_>, WorkspaceSessionError> {
        let mut creating = self
            .creating_sessions
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)?;
        let sessions = self.lock_sessions()?;
        let destroy_in_flight = self
            .destroy_flights
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)?
            .contains_key(&workspace_session_id);
        if creating.contains(&workspace_session_id)
            || sessions.contains_key(&workspace_session_id)
            || destroy_in_flight
        {
            return Err(WorkspaceSessionError::DuplicateWorkspaceSessionId {
                workspace_session_id,
            });
        }
        drop(sessions);
        creating.insert(workspace_session_id.clone());
        Ok(CreateReservation {
            service: self,
            workspace_session_id,
        })
    }

    /// Bind the one command teardown owner for these sessions. The weak edge
    /// avoids a service cycle while still letting a holder-exit transaction
    /// drain every admitted command before namespace resources are released.
    pub(crate) fn register_command_teardown(&self, teardown: &Arc<dyn WorkspaceCommandTeardown>) {
        *self
            .command_teardown
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(teardown));
    }

    /// Cancel and join the exact command ids recorded in a dead workspace.
    /// There is one deadline for the whole set, so a large ledger cannot turn
    /// teardown into an unbounded sequence of per-command waits.
    pub(crate) fn cancel_and_join_commands(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_ids: &[NamespaceExecutionId],
    ) -> Result<(), String> {
        if command_ids.is_empty() {
            return Ok(());
        }
        let teardown = self
            .command_teardown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(|| "command teardown owner is unavailable".to_owned())?;
        teardown.cancel_and_join(workspace_session_id, command_ids)
    }

    pub(crate) fn release_terminal_commands(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<WorkspaceCommandReleaseProof, String> {
        let teardown = self
            .command_teardown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade);
        teardown.map_or_else(
            || {
                Ok(WorkspaceCommandReleaseProof::new(
                    workspace_session_id.clone(),
                    0,
                ))
            },
            |owner| owner.release_for_destroy(workspace_session_id),
        )
    }
}

fn write_workload_cgroup_limits(
    path: &std::path::Path,
    limits: WorkloadCgroupLimits,
) -> Result<(), std::io::Error> {
    const CPU_PERIOD_US: u128 = 100_000;
    const NANOS_PER_CPU: u128 = 1_000_000_000;
    let quota = (u128::from(limits.nano_cpus) * CPU_PERIOD_US).div_ceil(NANOS_PER_CPU);
    let quota = u64::try_from(quota).unwrap_or(u64::MAX).max(1);
    std::fs::write(path.join("cpu.max"), format!("{quota} {CPU_PERIOD_US}"))?;
    std::fs::write(
        path.join("memory.high"),
        limits.memory_high_bytes.to_string(),
    )?;
    std::fs::write(path.join("memory.max"), limits.memory_max_bytes.to_string())?;
    std::fs::write(path.join("memory.oom.group"), "1")?;
    std::fs::write(path.join("pids.max"), limits.pids_max.to_string())?;
    Ok(())
}

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{unmount, UnmountFlags};
use sandbox_observability_telemetry::Observer;
use sandbox_runtime_layerstack::service::{LayerStackRouteSnapshot, StackObservation};

use crate::command::CommandOperationService;
use crate::file::FileService;
use crate::layerstack::LayerStackService;
use crate::observability::{
    RuntimeObservabilitySnapshot, RuntimeOwnershipSnapshot, RuntimeOwnershipTopologySnapshot,
};
use crate::workspace_crate::{
    session::WorkspaceManager, WorkspaceRuntimeService, WorkspaceStorageMode,
};
use crate::workspace_session::{
    HolderExitDispatcher, WorkspaceSessionService, WorkspaceSessionShutdownOutcome,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeShutdownPhase {
    Autosquash,
    HolderExitDispatcher,
    WorkspaceSession,
    CommandSupervisor,
    WorkspaceRuntime,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeShutdownFailure {
    pub phase: RuntimeShutdownPhase,
    pub workspace_session_id: Option<crate::workspace_crate::WorkspaceSessionId>,
    pub diagnostic: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeShutdownReport {
    pub sessions_observed: usize,
    pub sessions_converged: usize,
    pub failures: Vec<RuntimeShutdownFailure>,
}

impl RuntimeShutdownReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

enum RuntimeShutdownState {
    Open,
    Running,
    Complete(RuntimeShutdownReport),
}

struct RuntimeShutdownCoordinator {
    state: Mutex<RuntimeShutdownState>,
    ready: Condvar,
}

impl RuntimeShutdownCoordinator {
    fn new() -> Self {
        Self {
            state: Mutex::new(RuntimeShutdownState::Open),
            ready: Condvar::new(),
        }
    }

    fn begin(&self) -> Option<RuntimeShutdownReport> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match &*state {
                RuntimeShutdownState::Open => {
                    *state = RuntimeShutdownState::Running;
                    return None;
                }
                RuntimeShutdownState::Running => {
                    state = self
                        .ready
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                RuntimeShutdownState::Complete(report) => return Some(report.clone()),
            }
        }
    }

    fn complete(&self, report: RuntimeShutdownReport) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) =
            RuntimeShutdownState::Complete(report);
        self.ready.notify_all();
    }
}

#[derive(Default)]
struct RuntimeShutdownReportBuilder {
    observed: HashSet<crate::workspace_crate::WorkspaceSessionId>,
    converged: HashSet<crate::workspace_crate::WorkspaceSessionId>,
    session_failures: HashMap<crate::workspace_crate::WorkspaceSessionId, String>,
    session_registry_failure: Option<String>,
    other_failures: Vec<RuntimeShutdownFailure>,
}

impl RuntimeShutdownReportBuilder {
    fn record_sessions(&mut self, outcomes: Vec<WorkspaceSessionShutdownOutcome>) {
        self.session_registry_failure = None;
        for outcome in outcomes {
            self.observed.insert(outcome.workspace_session_id.clone());
            match outcome.result {
                Ok(()) => {
                    self.converged.insert(outcome.workspace_session_id.clone());
                    self.session_failures.remove(&outcome.workspace_session_id);
                }
                Err(diagnostic) => {
                    self.converged.remove(&outcome.workspace_session_id);
                    self.session_failures
                        .insert(outcome.workspace_session_id, diagnostic);
                }
            }
        }
    }

    fn failure(&mut self, phase: RuntimeShutdownPhase, diagnostic: String) {
        self.other_failures.push(RuntimeShutdownFailure {
            phase,
            workspace_session_id: None,
            diagnostic,
        });
    }

    fn session_registry_failure(&mut self, diagnostic: String) {
        self.session_registry_failure = Some(diagnostic);
    }

    fn finish(self) -> RuntimeShutdownReport {
        let mut failures = self
            .session_failures
            .into_iter()
            .map(
                |(workspace_session_id, diagnostic)| RuntimeShutdownFailure {
                    phase: RuntimeShutdownPhase::WorkspaceSession,
                    workspace_session_id: Some(workspace_session_id),
                    diagnostic,
                },
            )
            .collect::<Vec<_>>();
        failures.sort_by(|left, right| {
            left.workspace_session_id
                .as_ref()
                .map(|id| id.0.as_str())
                .cmp(&right.workspace_session_id.as_ref().map(|id| id.0.as_str()))
        });
        if let Some(diagnostic) = self.session_registry_failure {
            failures.push(RuntimeShutdownFailure {
                phase: RuntimeShutdownPhase::WorkspaceSession,
                workspace_session_id: None,
                diagnostic,
            });
        }
        failures.extend(self.other_failures);
        RuntimeShutdownReport {
            sessions_observed: self.observed.len(),
            sessions_converged: self.converged.len(),
            failures,
        }
    }
}

#[derive(Clone)]
pub struct SandboxRuntimeOperations {
    _holder_exit_dispatcher: Option<Arc<HolderExitDispatcher>>,
    pub command: Arc<CommandOperationService>,
    pub workspace_session: Arc<WorkspaceSessionService>,
    pub layerstack: Arc<LayerStackService>,
    pub file: Arc<FileService>,
    _autosquash_engine: Arc<crate::layerstack::autosquash_engine::AutosquashEngine>,
    shutdown: Arc<RuntimeShutdownCoordinator>,
}

impl SandboxRuntimeOperations {
    #[must_use]
    pub fn new(
        command: Arc<CommandOperationService>,
        workspace_session: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
        file: Arc<FileService>,
    ) -> Self {
        let holder_exit_dispatcher = HolderExitDispatcher::start(&workspace_session)
            .expect("holder exit dispatcher initialization failed");
        let autosquash_engine = Arc::new(
            crate::layerstack::autosquash_engine::AutosquashEngine::start(
                Arc::clone(&layerstack),
                Arc::clone(&workspace_session),
            ),
        );
        Self {
            _holder_exit_dispatcher: holder_exit_dispatcher,
            command,
            workspace_session,
            layerstack,
            file,
            _autosquash_engine: autosquash_engine,
            shutdown: Arc::new(RuntimeShutdownCoordinator::new()),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn autosquash_worker_threads(&self) -> usize {
        self._autosquash_engine.worker_threads()
    }

    /// Stop every operation-owned background worker and converge all runtime
    /// resources. Concurrent and repeated callers receive the same report.
    #[must_use]
    pub fn shutdown(&self) -> RuntimeShutdownReport {
        if let Some(report) = self.shutdown.begin() {
            return report;
        }
        let report =
            catch_unwind(AssertUnwindSafe(|| self.perform_shutdown())).unwrap_or_else(|_| {
                RuntimeShutdownReport {
                    sessions_observed: 0,
                    sessions_converged: 0,
                    failures: vec![RuntimeShutdownFailure {
                        phase: RuntimeShutdownPhase::Internal,
                        workspace_session_id: None,
                        diagnostic: "runtime shutdown panicked".to_owned(),
                    }],
                }
            });
        self.shutdown.complete(report.clone());
        report
    }

    fn perform_shutdown(&self) -> RuntimeShutdownReport {
        let mut report = RuntimeShutdownReportBuilder::default();
        if catch_unwind(AssertUnwindSafe(|| {
            self._autosquash_engine.shutdown_and_join();
        }))
        .is_err()
        {
            report.failure(
                RuntimeShutdownPhase::Autosquash,
                "autosquash shutdown panicked".to_owned(),
            );
        }
        if let Some(dispatcher) = &self._holder_exit_dispatcher {
            if catch_unwind(AssertUnwindSafe(|| dispatcher.shutdown_and_join())).is_err() {
                report.failure(
                    RuntimeShutdownPhase::HolderExitDispatcher,
                    "holder-exit dispatcher shutdown panicked".to_owned(),
                );
            }
        }

        self.converge_sessions(&mut report);
        match catch_unwind(AssertUnwindSafe(|| self.command.shutdown_and_join())) {
            Ok(Ok(())) => {}
            Ok(Err(diagnostic)) => {
                report.failure(RuntimeShutdownPhase::CommandSupervisor, diagnostic);
            }
            Err(_) => report.failure(
                RuntimeShutdownPhase::CommandSupervisor,
                "command supervisor shutdown panicked".to_owned(),
            ),
        }
        self.converge_sessions(&mut report);

        match catch_unwind(AssertUnwindSafe(|| {
            let first = self.workspace_session.workspace().shutdown();
            if first.is_complete() {
                first
            } else {
                self.workspace_session.workspace().shutdown()
            }
        })) {
            Ok(raw) if raw.is_complete() => {}
            Ok(raw) => report.failure(
                RuntimeShutdownPhase::WorkspaceRuntime,
                format!(
                    "raw workspace shutdown incomplete: remaining={}, retryable_failures={}, namespace_stopped={}, namespace_error={}",
                    raw.workspaces.remaining_workspace_ids.len(),
                    raw.workspaces.retryable_failures.len(),
                    raw.namespace_stopped,
                    raw.namespace_error.as_deref().unwrap_or("none")
                ),
            ),
            Err(_) => report.failure(
                RuntimeShutdownPhase::WorkspaceRuntime,
                "raw workspace shutdown panicked".to_owned(),
            ),
        }
        report.finish()
    }

    fn converge_sessions(&self, report: &mut RuntimeShutdownReportBuilder) {
        match catch_unwind(AssertUnwindSafe(|| {
            self.workspace_session.shutdown_sessions()
        })) {
            Ok(Ok(outcomes)) => report.record_sessions(outcomes),
            Ok(Err(diagnostic)) => report.session_registry_failure(diagnostic),
            Err(_) => report.session_registry_failure("session shutdown panicked".to_owned()),
        }
    }

    /// Assemble the runtime services over one shared process `Observer` (a clone
    /// of the daemon's). Every emitting service holds that same handle, so daemon
    /// and runtime spans share one id sequence and one parent chain.
    #[must_use]
    pub fn from_config(config: SandboxRuntimeConfig, observer: Observer) -> Self {
        let layer_stack_root = config.workspace.layer_stack_root.clone();
        let workspace_scratch_root = config.workspace.scratch_root.clone();
        let mpla_lifecycle_roots = crate::workspace_session::MplaLifecycleRoots {
            payload_root: layer_stack_root.join("mpla-poc/payload"),
            control_root: workspace_scratch_root.join("mpla-poc/control"),
            storage_admin_profile: config.mpla_storage_admin_profile,
        };
        let scratch_locator =
            crate::workspace_crate::WorkspaceScratchLocator::new(workspace_scratch_root.clone())
                .expect("workspace scratch locator initialization failed");
        let legacy_scratch_root = config.namespace_execution.scratch_root.clone();
        let legacy_scratch_locator = legacy_scratch_root.clone().map(|root| {
            crate::workspace_crate::LegacyExecutionScratchLocator::new(root)
                .expect("legacy compatibility scratch locator initialization failed")
        });
        let file = Arc::new(
            FileService::open(file_auditability_dir(&layer_stack_root), config.file)
                .expect("file auditability store initialization failed"),
        );
        let workspace_storage_mode = match config.layerstack.rollout_mode {
            sandbox_runtime_layerstack::service::StorageRolloutMode::Legacy
            | sandbox_runtime_layerstack::service::StorageRolloutMode::Validation => {
                WorkspaceStorageMode::Legacy
            }
            sandbox_runtime_layerstack::service::StorageRolloutMode::StrictCandidate => {
                WorkspaceStorageMode::StrictCandidate {
                    admission_lease_ttl: Duration::from_secs(60),
                    session_lease_ttl: Duration::from_secs(u64::from(u32::MAX)),
                }
            }
        };
        let workspace_runtime = Arc::new(WorkspaceRuntimeService::new_with_storage_mode(
            WorkspaceManager::with_scratch_locator(
                config
                    .workspace
                    .workspace_root
                    .to_string_lossy()
                    .into_owned(),
                config.workspace.caps.clone().into(),
                scratch_locator.clone(),
                observer.clone(),
            ),
            layer_stack_root.clone(),
            workspace_storage_mode,
        ));
        cli_log(format!(
            "ensuring workspace base for {}",
            config.workspace.workspace_root.display()
        ));
        let base_result = sandbox_runtime_layerstack::ensure_workspace_base(
            &layer_stack_root,
            &config.workspace.workspace_root,
        );
        match base_result {
            Ok((_binding, built)) => cli_log(if built {
                "workspace base built"
            } else {
                "workspace base already exists"
            }),
            Err(error) => {
                cli_log(error.to_string());
                panic!("layerstack workspace base initialization failed: {error}");
            }
        }
        detach_workspace_bind_after_base(&config.workspace.workspace_root);
        let layerstack = Arc::new(
            LayerStackService::new(
                layer_stack_root,
                workspace_scratch_root.clone(),
                config.layerstack,
                observer.clone(),
                Arc::clone(&file),
            )
            .expect("layerstack service initialization failed"),
        );
        let workspace_session = Arc::new(
            match (config.cgroup_root.clone(), config.workload_cgroup_limits) {
                (Some(cgroup_root), Some(limits)) => WorkspaceSessionService::with_workload_cgroup(
                    workspace_runtime,
                    Arc::clone(&layerstack),
                    cgroup_root,
                    limits,
                    observer.clone(),
                ),
                (None, Some(limits)) => WorkspaceSessionService::with_unavailable_workload_cgroup(
                    workspace_runtime,
                    Arc::clone(&layerstack),
                    limits,
                    config
                        .workload_cgroup_unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "delegated cgroup v2 root is unavailable".to_owned()),
                    observer.clone(),
                ),
                (cgroup_root, _) => WorkspaceSessionService::with_cgroup_root(
                    workspace_runtime,
                    Arc::clone(&layerstack),
                    cgroup_root,
                    observer.clone(),
                ),
            }
            .with_mpla_lifecycle_roots(mpla_lifecycle_roots),
        );
        let command = Arc::new(CommandOperationService::new_with_locators(
            Arc::clone(&workspace_session),
            crate::command::CommandConfig {
                scratch_root: workspace_scratch_root,
                max_active: config.command.max_active,
                setup_timeout_s: config.workspace.caps.setup_timeout_s,
                read_lines_default: config.command.read_lines_default,
                read_lines_max: config.command.read_lines_max,
                execution: config.namespace_execution.caps,
            },
            scratch_locator,
            legacy_scratch_locator,
            observer.clone(),
        ));
        let legacy_reap = crate::workspace_scratch_compat::reap_legacy_execution_scratch(
            legacy_scratch_root.as_deref(),
            &std::collections::HashSet::new(),
            std::time::SystemTime::now(),
            crate::workspace_scratch_compat::LEGACY_REAP_MIN_AGE,
        );
        command.record_legacy_scratch_reap(legacy_reap.clone());
        observer.event(
            "legacy_execution_scratch_reap",
            serde_json::json!({
                "root_configured": legacy_reap.root_configured,
                "scanned_entries": legacy_reap.scanned_entries,
                "deleted": legacy_reap.deleted,
                "skipped_active": legacy_reap.skipped_active,
                "skipped_recent": legacy_reap.skipped_recent,
                "skipped_unsafe": legacy_reap.skipped_unsafe,
                "errors": legacy_reap.errors,
                "saturated": legacy_reap.saturated,
            }),
        );
        boot_remove_export_spools(&layerstack);
        boot_reap_then_sweep(&workspace_session, &layerstack, &observer);
        Self::new(command, workspace_session, layerstack, file)
    }

    #[must_use]
    pub fn observability_snapshot(&self) -> RuntimeObservabilitySnapshot {
        let (workspaces, mut partial_errors) = self.workspace_session.snapshot_workspaces();
        let active_namespace_executions = self.command.active_namespace_executions();
        let ownership = self.ownership_snapshot(&mut partial_errors);
        RuntimeObservabilitySnapshot {
            workspaces,
            active_namespace_executions,
            ownership,
            partial_errors,
        }
    }

    #[must_use]
    pub fn ownership_topology_snapshot(&self) -> RuntimeOwnershipTopologySnapshot {
        let (workspaces, mut partial_errors) =
            self.workspace_session.snapshot_topology_workspaces();
        let ownership = self.ownership_snapshot(&mut partial_errors);
        RuntimeOwnershipTopologySnapshot {
            workspaces,
            active_command_count: self.command.active_namespace_execution_count(),
            active_layer_lease_count: self.layerstack.active_lease_count(),
            ownership,
            partial_errors,
        }
    }

    fn ownership_snapshot(&self, partial_errors: &mut Vec<String>) -> RuntimeOwnershipSnapshot {
        let command = self.command.scratch_ownership_snapshot();
        match self.workspace_session.workspace().ownership_snapshot() {
            Ok(snapshot) => RuntimeOwnershipSnapshot {
                namespace_fd_count: Some(snapshot.namespace_fd_count),
                control_fd_count: Some(snapshot.control_fd_count),
                active_scratch_directories: Some(snapshot.active_scratch_directories),
                persisted_workspace_handles: Some(snapshot.persisted_workspace_handles),
                exited_unreaped_holders: Some(snapshot.exited_unreaped_holders),
                scratch_layout_version: Some(command.layout_version),
                scratch_route: Some(command.route.to_owned()),
                active_execution_scratch_leases: Some(command.active_leases),
                retained_terminal_records: Some(command.retained_terminal_records),
                open_transcript_descriptors: Some(command.open_transcript_descriptors),
                live_execution_scratch_bytes: Some(command.live_bytes),
                high_water_execution_scratch_bytes: Some(command.high_water_bytes),
                teardown_join_total: Some(command.teardown_join_total),
                teardown_deadline_total: Some(command.teardown_deadline_total),
                legacy_entries_scanned: Some(command.legacy.scanned_entries),
                legacy_entries_deleted: Some(command.legacy.deleted),
                legacy_entries_skipped_active: Some(command.legacy.skipped_active),
                legacy_entries_skipped_recent: Some(command.legacy.skipped_recent),
                legacy_entries_skipped_unsafe: Some(command.legacy.skipped_unsafe),
                scratch_cleanup_state: Some(command.cleanup_state.to_owned()),
                scratch_quiescent: Some(command.quiescent),
            },
            Err(error) => {
                partial_errors.push(format!("workspace ownership snapshot failed: {error}"));
                RuntimeOwnershipSnapshot {
                    scratch_layout_version: Some(command.layout_version),
                    scratch_route: Some(command.route.to_owned()),
                    active_execution_scratch_leases: Some(command.active_leases),
                    retained_terminal_records: Some(command.retained_terminal_records),
                    open_transcript_descriptors: Some(command.open_transcript_descriptors),
                    live_execution_scratch_bytes: Some(command.live_bytes),
                    high_water_execution_scratch_bytes: Some(command.high_water_bytes),
                    teardown_join_total: Some(command.teardown_join_total),
                    teardown_deadline_total: Some(command.teardown_deadline_total),
                    legacy_entries_scanned: Some(command.legacy.scanned_entries),
                    legacy_entries_deleted: Some(command.legacy.deleted),
                    legacy_entries_skipped_active: Some(command.legacy.skipped_active),
                    legacy_entries_skipped_recent: Some(command.legacy.skipped_recent),
                    legacy_entries_skipped_unsafe: Some(command.legacy.skipped_unsafe),
                    scratch_cleanup_state: Some(command.cleanup_state.to_owned()),
                    scratch_quiescent: Some(command.quiescent),
                    ..RuntimeOwnershipSnapshot::default()
                }
            }
        }
    }

    /// Live per-layer lease breakdown of the active manifest (in-memory state).
    ///
    /// The daemon merges this with the telemetry reader's disk byte
    /// sizes (keyed by layer id) to render the `layerstack` inventory.
    pub fn observe_layerstack(
        &self,
    ) -> Result<StackObservation, crate::layerstack::LayerStackServiceError> {
        self.layerstack.observe()
    }

    /// Return in-memory route accounting without reopening the active stack.
    #[must_use]
    pub fn observe_layerstack_route(&self) -> LayerStackRouteSnapshot {
        self.layerstack.observe_route()
    }

    /// Storage root of the layer stack, for the telemetry byte reader.
    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        self.layerstack.layer_stack_root()
    }
}

/// Export-owned boot step: remove `<scratch_root>/.export/` wholesale before
/// serving. The session boot reap is registry-driven and never walks scratch
/// for unknown directories, so a spool orphaned by a crashed export would
/// leak forever without this.
fn boot_remove_export_spools(layerstack: &Arc<LayerStackService>) {
    let spool_dir = layerstack.export_spool_dir();
    match std::fs::remove_dir_all(&spool_dir) {
        Ok(()) => cli_log(format!("export boot reap removed {}", spool_dir.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => cli_log(format!(
            "export boot reap failed for {}: {error}",
            spool_dir.display()
        )),
    }
}

/// Boot cleanup, once, before serving: assert the kernel floor, reap every
/// persisted session (each is provably dead — PDEATHSIG), then run the
/// fail-closed storage sweep. Reap records are emitted before any sweep
/// deletion record; both ride existing record names, so the feature's
/// record budget stays at three.
fn boot_reap_then_sweep(
    workspace_session: &Arc<WorkspaceSessionService>,
    layerstack: &Arc<LayerStackService>,
    observer: &Observer,
) {
    assert_kernel_floor();
    probe_and_set_remount_gate(layerstack, observer);
    let reaped = match workspace_session.workspace().reap_persisted_sessions() {
        Ok(reaped) => reaped,
        Err(error) => {
            observer.event(
                sandbox_observability_telemetry::record::names::WORKSPACE_SESSION_CLEANUP_FAILED,
                serde_json::json!({
                    "boot_reap": true,
                    "error": error.to_string(),
                }),
            );
            cli_log(format!("boot reap failed: {error}"));
            Vec::new()
        }
    };
    for session in &reaped {
        observer.event(
            sandbox_observability_telemetry::record::names::WORKSPACE_SESSION_DESTROY,
            serde_json::json!({
                "boot_reap": true,
                "workspace_handle_id": session.workspace_handle_id,
                "run_dir_removed": session.run_dir_removed,
                "lease_released": session.lease_released,
                "lease_release_error": session.lease_release_error,
                "run_dir_cleanup_error": session.run_dir_cleanup_error,
                "persisted_handle_released": session.persisted_handle_released,
            }),
        );
    }
    cli_log(format!(
        "boot reap removed {} dead session(s)",
        reaped.len()
    ));
    let sweep =
        sandbox_runtime_layerstack::LayerStack::open(layerstack.layer_stack_root().to_path_buf())
            .and_then(|mut stack| stack.sweep_storage());
    match sweep {
        Ok(report) => {
            observer.event(
                sandbox_observability_telemetry::record::names::LAYERSTACK_SQUASH,
                serde_json::json!({
                    "boot_sweep": true,
                    "removed_layer_ids": report.removed_layer_ids,
                    "removed_staging_entries": report.removed_staging_entries,
                    "skipped_reason": report.skipped_reason,
                }),
            );
            cli_log(format!(
                "boot storage sweep: removed {} layer id(s), {} staging entries{}",
                report.removed_layer_ids.len(),
                report.removed_staging_entries,
                report
                    .skipped_reason
                    .map(|reason| format!(", skipped: {reason}"))
                    .unwrap_or_default()
            ));
        }
        Err(error) => cli_log(format!("boot storage sweep failed: {error}")),
    }
}

/// Probe the same-upperdir / userxattr kernel gate once and flip live
/// remount on only if it holds; otherwise squash stays commit-only and every
/// session reports `leased(unsupported:kernel_gate_not_proven)`.
fn probe_and_set_remount_gate(layerstack: &Arc<LayerStackService>, observer: &Observer) {
    // The probe mounts a scratch overlay, so its scratch must be on a real
    // (non-overlay) filesystem — the layer-stack volume is ext4, unlike the
    // container's overlay rootfs at /eos.
    let scratch = layerstack.layer_stack_root().join("staging");
    let proven = crate::workspace_crate::probe_and_set_live_remount_gate(&scratch);
    observer.event(
        sandbox_observability_telemetry::record::names::NAMESPACE_EXEC_REMOUNT_OVERLAY,
        serde_json::json!({ "boot_gate": true, "live_remount_enabled": proven }),
    );
    cli_log(format!(
        "live remount kernel gate: {}",
        if proven {
            "PROVEN (enabled)"
        } else {
            "NOT PROVEN (squash commit-only)"
        }
    ));
}

/// The supported daemon environment is Linux ≥ 5.8 (`syncfs` writeback error
/// reporting); refuse to serve on anything older.
#[cfg(target_os = "linux")]
fn assert_kernel_floor() {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut parts = release.trim().split(['.', '-']);
    let major: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    assert!(
        (major, minor) >= (5, 8),
        "unsupported kernel {release}: the sandbox daemon requires Linux >= 5.8"
    );
}

#[cfg(not(target_os = "linux"))]
fn assert_kernel_floor() {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceBindDetach {
    #[cfg(target_os = "linux")]
    Unmounted,
    NotMounted,
}

fn detach_workspace_bind_after_base(workspace_root: &Path) {
    cli_log(format!(
        "unmounting workspace bind {}",
        workspace_root.display()
    ));
    match detach_workspace_bind(workspace_root) {
        #[cfg(target_os = "linux")]
        Ok(WorkspaceBindDetach::Unmounted) => cli_log(format!(
            "workspace bind unmounted {}",
            workspace_root.display()
        )),
        Ok(WorkspaceBindDetach::NotMounted) => cli_log(format!(
            "workspace bind not mounted {}",
            workspace_root.display()
        )),
        Err(error) => {
            cli_log(format!(
                "workspace bind unmount failed {}: {error}",
                workspace_root.display()
            ));
            panic!(
                "workspace bind unmount failed for {}: {error}",
                workspace_root.display()
            );
        }
    }
    if !workspace_root.is_dir() {
        let message = format!(
            "workspace mountpoint missing after unmount {}",
            workspace_root.display()
        );
        cli_log(&message);
        panic!("{message}");
    }
}

#[cfg(target_os = "linux")]
fn detach_workspace_bind(workspace_root: &Path) -> Result<WorkspaceBindDetach, std::io::Error> {
    match unmount(workspace_root, UnmountFlags::empty()) {
        Ok(()) => Ok(WorkspaceBindDetach::Unmounted),
        Err(Errno::INVAL) => Ok(WorkspaceBindDetach::NotMounted),
        Err(error) => Err(std::io::Error::from(error)),
    }
}

#[cfg(not(target_os = "linux"))]
fn detach_workspace_bind(_workspace_root: &Path) -> Result<WorkspaceBindDetach, std::io::Error> {
    Ok(WorkspaceBindDetach::NotMounted)
}

fn cli_log(message: impl AsRef<str>) {
    let escaped = serde_json::to_string(message.as_ref()).unwrap_or_else(|_| "\"\"".to_owned());
    eprintln!("cli_log({escaped})");
}

/// The file-auditability log lives beside the layer stack, under
/// `<layer_stack_root>/../storage/file_auditability` (C3 spec §7.1) — the only
/// root this crate can reach from `config.workspace.layer_stack_root`.
fn file_auditability_dir(layer_stack_root: &Path) -> std::path::PathBuf {
    layer_stack_root
        .parent()
        .unwrap_or(layer_stack_root)
        .join("storage")
        .join("file_auditability")
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxRuntimeConfig {
    pub workspace: WorkspaceRuntimeConfig,
    pub namespace_execution: NamespaceExecutionRuntimeConfig,
    pub layerstack: LayerstackRuntimeConfig,
    pub command: CommandRuntimeConfig,
    pub file: FileRuntimeConfig,
    pub cgroup_root: Option<std::path::PathBuf>,
    pub workload_cgroup_limits: Option<WorkloadCgroupLimits>,
    pub workload_cgroup_unavailable_reason: Option<String>,
    /// Immutable daemon policy consumed by the fixed MPLA storage helper.
    /// It is never populated from a public operation request.
    pub mpla_storage_admin_profile:
        sandbox_runtime_mpla_poc::storage_admin::StorageAdminCapabilityProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadCgroupLimits {
    pub nano_cpus: u64,
    pub memory_high_bytes: u64,
    pub memory_max_bytes: u64,
    pub pids_max: u64,
}

/// Command-operation caps injected by the daemon from `runtime.command`;
/// `Default` preserves the shipped policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRuntimeConfig {
    pub max_active: usize,
    pub read_lines_default: usize,
    pub read_lines_max: usize,
}

impl Default for CommandRuntimeConfig {
    fn default() -> Self {
        Self {
            max_active: 32,
            read_lines_default: 200,
            read_lines_max: 1000,
        }
    }
}

/// File-operation caps injected by the daemon from `runtime.file`; `Default`
/// preserves the shipped policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRuntimeConfig {
    pub read_lines_default: usize,
    pub max_output_bytes: usize,
    pub max_edit_bytes: usize,
    pub max_list_entries: usize,
}

impl Default for FileRuntimeConfig {
    fn default() -> Self {
        Self {
            read_lines_default: 2000,
            max_output_bytes: 256 * 1024,
            max_edit_bytes: 4 * 1024 * 1024,
            max_list_entries: 2000,
        }
    }
}

/// Layer-stack tuning injected by the daemon from `runtime.layerstack`;
/// `Default` preserves the shipped policy for callers without that section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerstackRuntimeConfig {
    pub rollout_mode: sandbox_runtime_layerstack::service::StorageRolloutMode,
    pub remount_sweep_width: usize,
    pub export_chunk_bytes: u64,
    pub spool_zstd_level: i32,
    pub autosquash_squash_at_n_layers: Option<usize>,
}

impl Default for LayerstackRuntimeConfig {
    fn default() -> Self {
        Self {
            rollout_mode: sandbox_runtime_layerstack::service::StorageRolloutMode::Legacy,
            remount_sweep_width: 4,
            export_chunk_bytes: 2 * 1024 * 1024,
            spool_zstd_level: 3,
            autosquash_squash_at_n_layers: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRuntimeConfig {
    pub workspace_root: std::path::PathBuf,
    pub layer_stack_root: std::path::PathBuf,
    pub scratch_root: std::path::PathBuf,
    pub caps: WorkspaceResourceCaps,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceExecutionRuntimeConfig {
    pub scratch_root: Option<std::path::PathBuf>,
    pub caps: NamespaceExecutionCaps,
}

/// Namespace-execution caps injected by the daemon from
/// `runtime.namespace_execution`; `Default` preserves the shipped policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NamespaceExecutionCaps {
    pub freeze_budget_s: f64,
    pub stdin_write_deadline_s: f64,
    pub max_terminal_entries: usize,
    pub max_transcript_window_bytes: u64,
    pub max_runner_result_bytes: usize,
    pub command_security_profile: crate::CommandSecurityProfile,
}

impl Default for NamespaceExecutionCaps {
    fn default() -> Self {
        Self {
            freeze_budget_s: 0.5,
            stdin_write_deadline_s: 2.0,
            max_terminal_entries: 512,
            max_transcript_window_bytes: 1024 * 1024,
            max_runner_result_bytes: 8 * 1024 * 1024,
            command_security_profile: crate::CommandSecurityProfile::Standard,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceResourceCaps {
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
    pub freeze_budget_s: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

impl From<WorkspaceResourceCaps> for crate::workspace_crate::session::ResourceCaps {
    fn from(caps: WorkspaceResourceCaps) -> Self {
        Self {
            setup_timeout_s: caps.setup_timeout_s,
            exit_grace_s: caps.exit_grace_s,
            rfc1918_egress: match caps.rfc1918_egress {
                Rfc1918Egress::Allow => crate::workspace_crate::session::Rfc1918Egress::Allow,
                Rfc1918Egress::Deny => crate::workspace_crate::session::Rfc1918Egress::Deny,
            },
            freeze_budget_s: caps.freeze_budget_s,
        }
    }
}

//! Isolated-workspace runtime: lease custody, command lifecycle, idle workspace eviction
//! policy, and the caller-keyed workspace-run cancel coordinator.
//!
//! The daemon composes this service: it parses wire args, calls one
//! [`WorkspaceRuntime`] method, and shapes one response. This crate owns the
//! cross-domain workspace-run policy: when leases are acquired and released,
//! when commands and handles are torn down, and in what order — while namespace mechanics
//! stay in `workspace` and command internals stay in
//! `operation::command`. State lives on a [`WorkspaceRuntime`] instance, never in
//! process globals.
//!
//! Lock-order discipline: caller workspace-mode transitions use `mode_gate` to
//! serialize command start with isolated-workspace enter/exit. The workspace
//! state lock is never held across command registry mutation.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use config::configs::isolated_workspace::{
    IsolatedWorkspaceConfig, Rfc1918Egress as ConfigRfc1918Egress,
};
use layerstack::{
    read_workspace_binding,
    service::{
        compact_snapshot_for_remount, LeaseReleaseHandle,
        LeaseReleaseReport as LayerStackLeaseReleaseReport, Snapshot, SnapshotNormalization,
    },
    LayerStack, LeaseAwareCopyThroughOutcome, WorkspaceBinding, WORKSPACE_BINDING_FILE,
};
use operation::command::{CommandOps, CommandRemountInspection, ExecTarget, HostCommandWorkspace};
use operation::isolation::contract::IsolationTestRemountFault;
use serde_json::Value;
use workspace::model::{
    BaseRevision, CallerId, CaptureChangesRequest, CaptureChangesResult, ChangedPathKind,
    NetworkMode, ProtectedPathDrop, WorkspaceHandle as UnifiedWorkspaceHandle, WorkspaceId,
};
use workspace::{capture_upperdir_with_payloads, IsolatedWorkspaceBinding};
use workspace::{
    EphemeralWorkspace, IsolatedError, IsolatedManager, IsolatedSnapshot, RemountProbe,
    ResourceCaps, Rfc1918Egress as RuntimeRfc1918Egress, WorkspaceError,
    WorkspaceHandle as IsolatedWorkspaceHandle,
};

const PERSISTED_HANDLES_SCHEMA_VERSION: u64 = 1;
const WORKSPACE_BINDING_SEARCH_DEPTH: usize = 4;

fn setup_error(error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::SetupFailed {
        step: error.to_string(),
    }
}

fn forced_remount_block_inspection(
    mut inspection: CommandRemountInspection,
    fault: IsolationTestRemountFault,
) -> CommandRemountInspection {
    inspection.blocked_reason = Some(fault.reason());
    inspection.detail = Some(format!("test forced {}", fault.reason()));
    match fault {
        IsolationTestRemountFault::ProcessMembershipChanged => {
            inspection.inspected = false;
        }
        IsolationTestRemountFault::MountinfoMismatch => {
            inspection.inspected = true;
            inspection.mountinfo_checked_count = inspection.mountinfo_checked_count.max(1);
        }
    }
    inspection
}

fn snapshot_normalization(outcome: LeaseAwareCopyThroughOutcome) -> SnapshotNormalization {
    SnapshotNormalization {
        triggered: outcome.manifest.is_some(),
        protected_layer_count: outcome.protected_layer_count,
        checkpoint_count: outcome.checkpoint_count,
        removed_layer_count: outcome.removed_layer_count,
        bytes_added: outcome.bytes_added,
        protected_pinned_bytes: outcome.protected_pinned_bytes,
        active_depth_before: outcome.active_depth_before,
        active_depth_after: outcome.active_depth_after,
    }
}

struct BoundState {
    workspace_root: PathBuf,
    layer_stack_root: PathBuf,
    stack: LayerStack,
    manager: IsolatedManager,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedWorkspaceRoot {
    pub workspace_root: PathBuf,
    pub layer_stack_root: PathBuf,
    pub binding: WorkspaceBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceRouteTraceFacts {
    pub kind: &'static str,
    pub reason: &'static str,
    pub layer_stack_root: Option<PathBuf>,
}

pub(crate) struct WorkspaceCommandRouteContext<'a> {
    route: WorkspaceCommandRoute,
    trace: WorkspaceRouteTraceFacts,
    _mode_guard: MutexGuard<'a, ()>,
}

enum WorkspaceCommandRoute {
    Host {
        caller_id: String,
        workspace: HostWorkspaceLifecycle,
    },
    Isolated {
        binding: IsolatedWorkspaceBinding,
    },
}

impl WorkspaceCommandRouteContext<'_> {
    #[must_use]
    pub(crate) fn trace_facts(&self) -> &WorkspaceRouteTraceFacts {
        &self.trace
    }

    #[must_use]
    pub(crate) fn caller_id(&self) -> &str {
        match &self.route {
            WorkspaceCommandRoute::Host { caller_id, .. } => caller_id,
            WorkspaceCommandRoute::Isolated { binding } => &binding.caller_id,
        }
    }

    #[must_use]
    pub(crate) fn remountable(&self, requested: bool) -> bool {
        match &self.route {
            WorkspaceCommandRoute::Host { .. } => false,
            WorkspaceCommandRoute::Isolated { .. } => requested,
        }
    }

    pub(crate) fn with_exec_target<T>(
        self,
        scratch_root: PathBuf,
        exec: impl FnOnce(ExecTarget) -> T,
    ) -> T {
        let Self {
            route,
            trace: _trace,
            _mode_guard,
        } = self;
        let target = match route {
            WorkspaceCommandRoute::Host { workspace, .. } => ExecTarget::Ephemeral {
                workspace: Box::new(workspace.into_command_workspace()),
                scratch_root,
            },
            WorkspaceCommandRoute::Isolated { binding } => ExecTarget::Isolated {
                binding: Box::new(binding),
            },
        };
        exec(target)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum WorkspaceFileRouteContext {
    Direct { layer_stack_root: PathBuf },
    Isolated { binding: IsolatedWorkspaceBinding },
}

impl WorkspaceFileRouteContext {
    #[must_use]
    pub(crate) fn trace_facts(&self) -> WorkspaceRouteTraceFacts {
        match self {
            Self::Direct { layer_stack_root } => WorkspaceRouteTraceFacts {
                kind: "fast_path",
                reason: "no_isolated_workspace_for_caller",
                layer_stack_root: Some(layer_stack_root.clone()),
            },
            Self::Isolated { .. } => WorkspaceRouteTraceFacts {
                kind: "isolated_workspace",
                reason: "caller_has_open_isolated_workspace",
                layer_stack_root: None,
            },
        }
    }

    #[must_use]
    pub(crate) fn direct_layer_stack_root(&self) -> Option<&Path> {
        match self {
            Self::Direct { layer_stack_root } => Some(layer_stack_root),
            Self::Isolated { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeasedBaseRevision {
    pub lease_id: String,
    pub version: i64,
    pub root_hash: String,
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct HostWorkspaceLifecycle {
    #[cfg_attr(not(test), allow(dead_code))]
    pub caller_id: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub workspace_id: WorkspaceId,
    pub layer_stack_root: PathBuf,
    pub workspace_root: PathBuf,
    pub leased_base: LeasedBaseRevision,
    pub snapshot_normalization: SnapshotNormalization,
    pub workspace: EphemeralWorkspace,
    pub lease: LeaseReleaseHandle,
}

impl HostWorkspaceLifecycle {
    #[must_use]
    pub(crate) fn into_command_workspace(self) -> HostCommandWorkspace {
        let snapshot = Snapshot {
            lease_id: self.leased_base.lease_id,
            manifest_version: self.leased_base.version,
            root_hash: self.leased_base.root_hash,
            layer_paths: self.leased_base.layer_paths,
        };
        HostCommandWorkspace::new(
            self.layer_stack_root,
            self.workspace_root,
            snapshot,
            self.snapshot_normalization,
            self.workspace,
            self.lease,
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeaseReleaseReport {
    pub released: Option<bool>,
    pub error: Option<String>,
}

impl From<LayerStackLeaseReleaseReport> for LeaseReleaseReport {
    fn from(report: LayerStackLeaseReleaseReport) -> Self {
        Self {
            released: report.released,
            error: report.error,
        }
    }
}

impl BoundState {
    /// Acquire a snapshot lease for `caller_id` and shape it for `enter`.
    fn acquire_snapshot(
        &mut self,
        caller_id: &str,
        max_depth: usize,
    ) -> Result<(IsolatedSnapshot, SnapshotNormalization), IsolatedError> {
        let command_snapshot = self
            .stack
            .acquire_bounded_snapshot_for_command(&format!("isolated-{caller_id}"), max_depth)
            .map_err(setup_error)?;
        let normalization = snapshot_normalization(command_snapshot.copy_through);
        let lease = command_snapshot.lease;
        let snapshot = IsolatedSnapshot {
            lease_id: lease.lease_id,
            manifest_version: lease.manifest_version,
            manifest_root_hash: lease.root_hash,
            layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
        };
        Ok((snapshot, normalization))
    }

    /// Best-effort lease release; returns whether the lease was held and retains
    /// any release error for request-side tracing.
    fn release_lease(&mut self, lease_id: &str) -> LeaseReleaseReport {
        match self.stack.release_lease(lease_id) {
            Ok(released) => LeaseReleaseReport {
                released: Some(released),
                error: None,
            },
            Err(err) => LeaseReleaseReport {
                released: None,
                error: Some(err.to_string()),
            },
        }
    }

    /// Exit `caller_id`'s workspace and release its lease, shaping the typed
    /// outcome with the lease custody fields.
    fn exit_caller(
        &mut self,
        caller_id: &str,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedError> {
        let isolated = self.manager.exit(caller_id, grace_s)?;
        let lease_release = self.release_lease(&isolated.lease_id);
        let active_leases_after = self.stack.active_lease_count();
        Ok(ExitOutcome {
            isolated,
            lease_released: lease_release.released,
            lease_release_error: lease_release.error,
            active_leases_after,
        })
    }

    fn compact_remount_open_workspace_for_test(
        &mut self,
        caller_id: &str,
        probe: &RemountProbe,
        inspection: Option<&CommandRemountInspection>,
    ) -> Result<WorkspaceRemountCompactionReport, IsolatedError> {
        let before_manifest = self.stack.read_active_manifest().map_err(setup_error)?;
        let before_metrics = self.stack.storage_metrics().map_err(setup_error)?;
        let handle = self
            .manager
            .get_handle(caller_id)
            .ok_or(IsolatedError::NotOpen)?;
        let compaction = compact_snapshot_for_remount(
            &self.layer_stack_root,
            handle.manifest_version,
            &handle.layer_paths,
        )
        .map_err(setup_error)?;
        let compact_manifest_version = compaction.manifest.version;
        let compact_manifest_root_hash = layerstack::manifest_root_hash(&compaction.manifest);
        let remounted = self.manager.remount_with_layers(
            caller_id,
            compact_manifest_version,
            compact_manifest_root_hash,
            compaction.layer_paths.clone(),
            probe,
        )?;
        let mount_verified = remounted.handle.layer_paths == compaction.layer_paths
            && remounted.remount.mount_verified;
        let lease_retargeted = self
            .stack
            .retarget_lease_manifest(&handle.lease_id, compaction.manifest)
            .map_err(setup_error)?;
        let squash = self.stack.squash(1).map_err(setup_error)?;
        let after_manifest = self.stack.read_active_manifest().map_err(setup_error)?;
        let after_metrics = self.stack.storage_metrics().map_err(setup_error)?;
        Ok(WorkspaceRemountCompactionReport {
            before_manifest_depth: before_manifest.depth(),
            before_layer_dirs: before_metrics.layer_dirs,
            before_storage_bytes: before_metrics.storage_bytes,
            compacted_snapshot_layers: compaction.before_layer_count,
            remounted_layer_count: remounted.handle.layer_paths.len(),
            live_remount: inspection.is_some(),
            mount_verified,
            remount_staged_switch: remounted.remount.staged_switch,
            remount_staging_verified: remounted.remount.staging_verified,
            remount_rollback_unmounted: remounted.remount.rollback_unmounted,
            remount_rollback_unmount_error: remounted.remount.rollback_unmount_error,
            remount_mount_namespace: remounted.remount.mount_namespace,
            remount_mountinfo_fs_type: remounted.remount.mountinfo_fs_type,
            remount_mountinfo_lowerdir_count: remounted.remount.mountinfo_lowerdir_count,
            remount_mountinfo_lowerdir_expected_count: remounted
                .remount
                .mountinfo_lowerdir_expected_count,
            remount_mountinfo_lowerdir_count_matched: remounted
                .remount
                .mountinfo_lowerdir_count_matched,
            remount_mountinfo_lowerdir_verified: remounted.remount.mountinfo_lowerdir_verified,
            remount_probe_read_ok: remounted.remount.probe_read_ok,
            remount_probe_content_matched: remounted.remount.probe_content_matched,
            remount_probe_error: remounted.remount.probe_error,
            lease_retargeted,
            remountable_commands: inspection
                .map_or(0, |inspection| inspection.remountable_commands),
            process_count: inspection.map_or(0, |inspection| inspection.process_count),
            quiesced_process_count: inspection
                .map_or(0, |inspection| inspection.quiesced_process_count),
            pinned_cwd_count: inspection.map_or(0, |inspection| inspection.pinned_cwd_count),
            pinned_root_count: inspection.map_or(0, |inspection| inspection.pinned_root_count),
            pinned_fd_count: inspection.map_or(0, |inspection| inspection.pinned_fd_count),
            pinned_mapped_file_count: inspection
                .map_or(0, |inspection| inspection.pinned_mapped_file_count),
            mountinfo_checked_count: inspection
                .map_or(0, |inspection| inspection.mountinfo_checked_count),
            process_resumed: inspection.is_none_or(|inspection| inspection.resumed),
            squash_manifest_version: squash.manifest.map(|manifest| manifest.version),
            squash_lease_release_error: squash.lease_release_error.map(|err| err.to_string()),
            after_manifest_depth: after_manifest.depth(),
            after_layer_dirs: after_metrics.layer_dirs,
            after_storage_bytes: after_metrics.storage_bytes,
            active_leases_after: self.stack.active_lease_count(),
        })
    }

    fn blocked_remount_report_for_test(
        &mut self,
        caller_id: &str,
        inspection: CommandRemountInspection,
    ) -> Result<WorkspaceRemountBlockedReport, IsolatedError> {
        let before_manifest = self.stack.read_active_manifest().map_err(setup_error)?;
        let before_metrics = self.stack.storage_metrics().map_err(setup_error)?;
        let handle = self
            .manager
            .get_handle(caller_id)
            .ok_or(IsolatedError::NotOpen)?;
        let lease_layer_count = handle.layer_paths.len();
        let pinned_bytes = handle
            .layer_paths
            .iter()
            .map(|path| best_effort_file_bytes(path))
            .sum();
        let parent_prefix_bytes = handle
            .layer_paths
            .iter()
            .skip(1)
            .map(|path| best_effort_file_bytes(path))
            .sum();
        let lease_age_s = (handle.last_activity - handle.created_at).max(0.0);
        let after_manifest = self.stack.read_active_manifest().map_err(setup_error)?;
        let after_metrics = self.stack.storage_metrics().map_err(setup_error)?;
        Ok(WorkspaceRemountBlockedReport {
            lease_id: handle.lease_id,
            manifest_version: handle.manifest_version,
            reason: inspection.reason_or_default(),
            active_commands: inspection.active_commands,
            remountable_commands: inspection.remountable_commands,
            command_ids: inspection.command_ids,
            process_group_ids: inspection.process_group_ids,
            process_count: inspection.process_count,
            quiesced_process_count: inspection.quiesced_process_count,
            pinned_cwd_count: inspection.pinned_cwd_count,
            pinned_root_count: inspection.pinned_root_count,
            pinned_fd_count: inspection.pinned_fd_count,
            pinned_mapped_file_count: inspection.pinned_mapped_file_count,
            mountinfo_checked_count: inspection.mountinfo_checked_count,
            inspection_detail: inspection.detail,
            inspected: inspection.inspected,
            quiesce_attempted: inspection.quiesce_attempted,
            resumed: inspection.resumed,
            lease_age_s,
            lease_layer_count,
            parent_prefix_layer_count: lease_layer_count.saturating_sub(1),
            parent_prefix_bytes,
            pinned_bytes,
            before_manifest_depth: before_manifest.depth(),
            before_layer_dirs: before_metrics.layer_dirs,
            before_storage_bytes: before_metrics.storage_bytes,
            fallback_compaction_enabled: false,
            fallback_compaction_policy: "disabled_report_only",
            fallback_checkpoint_count: 0,
            fallback_compacted_layers: 0,
            fallback_skipped_delta_intervals: 0,
            after_manifest_depth: after_manifest.depth(),
            after_layer_dirs: after_metrics.layer_dirs,
            after_storage_bytes: after_metrics.storage_bytes,
            active_leases_after: self.stack.active_lease_count(),
        })
    }
}

/// Typed result of one isolated-workspace exit: the manager's teardown outcome
/// plus the lease custody fields the daemon adapter splices into the wire
/// inspection object.
pub struct ExitOutcome {
    /// The namespace/cgroup/scratch teardown outcome from the isolated manager.
    pub isolated: workspace::ExitOutcome,
    /// Whether the workspace's snapshot lease was still held at release.
    pub lease_released: Option<bool>,
    /// Lease release failure retained for audit-side trace emission.
    pub lease_release_error: Option<String>,
    /// Active leases remaining on the bound stack after release.
    pub active_leases_after: usize,
}

/// Outcome of tearing down one caller's workspace runs.
pub struct CallerCancel {
    /// Commands that were live at entry (now cancelled + discarded).
    pub cancelled_commands: usize,
    /// Isolated-workspace teardown result: the typed exit outcome if the
    /// caller was isolated, `Err(IsolatedError::NotOpen)` if it was ephemeral
    /// (or had no isolated workspace), or another `IsolatedError` on teardown
    /// failure.
    pub isolated: Result<ExitOutcome, IsolatedError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceRecoveryReport {
    pub attempted: bool,
    pub exited_callers: Vec<String>,
    pub manager_json_error: Option<String>,
    pub orphan_cleanup_error: Option<String>,
}

impl WorkspaceRecoveryReport {
    fn merge_orphan_cleanup_error(&mut self, error: Option<String>) {
        if self.orphan_cleanup_error.is_none() {
            self.orphan_cleanup_error = error;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct IdleWorkspaceEvictionReport {
    pub evicted: Vec<IdleWorkspaceEviction>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IdleWorkspaceEviction {
    pub caller_id: String,
    pub workspace_handle_id: String,
    pub lease_id: String,
    pub evicted_upperdir_bytes: u64,
    pub lifetime_s: f64,
    pub total_ms: f64,
    pub lease_release: LeaseReleaseReport,
    pub active_leases_after: usize,
}

pub(crate) struct WorkspaceEnterOutcome {
    pub handle: IsolatedWorkspaceHandle,
    pub recovery: WorkspaceRecoveryReport,
    pub snapshot_normalization: SnapshotNormalization,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WorkspaceRemountCompactionAttempt {
    Compacted(WorkspaceRemountCompactionReport),
    Blocked(WorkspaceRemountBlockedReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceRemountCompactionReport {
    pub before_manifest_depth: usize,
    pub before_layer_dirs: usize,
    pub before_storage_bytes: u64,
    pub compacted_snapshot_layers: usize,
    pub remounted_layer_count: usize,
    pub live_remount: bool,
    pub mount_verified: bool,
    pub remount_staged_switch: bool,
    pub remount_staging_verified: Option<bool>,
    pub remount_rollback_unmounted: Option<bool>,
    pub remount_rollback_unmount_error: Option<String>,
    pub remount_mount_namespace: Option<String>,
    pub remount_mountinfo_fs_type: Option<String>,
    pub remount_mountinfo_lowerdir_count: Option<usize>,
    pub remount_mountinfo_lowerdir_expected_count: Option<usize>,
    pub remount_mountinfo_lowerdir_count_matched: Option<bool>,
    pub remount_mountinfo_lowerdir_verified: Option<bool>,
    pub remount_probe_read_ok: Option<bool>,
    pub remount_probe_content_matched: Option<bool>,
    pub remount_probe_error: Option<String>,
    pub lease_retargeted: bool,
    pub remountable_commands: usize,
    pub process_count: usize,
    pub quiesced_process_count: usize,
    pub pinned_cwd_count: usize,
    pub pinned_root_count: usize,
    pub pinned_fd_count: usize,
    pub pinned_mapped_file_count: usize,
    pub mountinfo_checked_count: usize,
    pub process_resumed: bool,
    pub squash_manifest_version: Option<i64>,
    pub squash_lease_release_error: Option<String>,
    pub after_manifest_depth: usize,
    pub after_layer_dirs: usize,
    pub after_storage_bytes: u64,
    pub active_leases_after: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkspaceRemountBlockedReport {
    pub lease_id: String,
    pub manifest_version: i64,
    pub reason: &'static str,
    pub active_commands: usize,
    pub remountable_commands: usize,
    pub command_ids: Vec<String>,
    pub process_group_ids: Vec<i32>,
    pub process_count: usize,
    pub quiesced_process_count: usize,
    pub pinned_cwd_count: usize,
    pub pinned_root_count: usize,
    pub pinned_fd_count: usize,
    pub pinned_mapped_file_count: usize,
    pub mountinfo_checked_count: usize,
    pub inspection_detail: Option<String>,
    pub inspected: bool,
    pub quiesce_attempted: bool,
    pub resumed: bool,
    pub lease_age_s: f64,
    pub lease_layer_count: usize,
    pub parent_prefix_layer_count: usize,
    pub parent_prefix_bytes: u64,
    pub pinned_bytes: u64,
    pub before_manifest_depth: usize,
    pub before_layer_dirs: usize,
    pub before_storage_bytes: u64,
    pub fallback_compaction_enabled: bool,
    pub fallback_compaction_policy: &'static str,
    pub fallback_checkpoint_count: usize,
    pub fallback_compacted_layers: usize,
    pub fallback_skipped_delta_intervals: usize,
    pub after_manifest_depth: usize,
    pub after_layer_dirs: usize,
    pub after_storage_bytes: u64,
    pub active_leases_after: usize,
}

/// Failures from opening an isolated workspace through [`WorkspaceRuntime`].
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceEnterError {
    /// The caller has live commands and cannot switch workspace mode.
    #[error("cannot enter isolated workspace while commands are active")]
    ActiveCommands {
        /// Live commands for this caller.
        active_commands: usize,
    },
    /// The isolated-workspace lifecycle failed.
    #[error(transparent)]
    Isolated(#[from] IsolatedError),
    /// The isolated-workspace lifecycle failed after acquiring a lease, and the
    /// follow-up lease release may also have failed.
    #[error("{source}")]
    EnterFailed {
        /// The lifecycle error returned to the caller.
        #[source]
        source: IsolatedError,
        /// Best-effort lease release report for trace emission.
        lease_release: LeaseReleaseReport,
    },
    /// The caller-facing workspace root could not be resolved to a LayerStack
    /// binding.
    #[error(transparent)]
    RootResolution(#[from] WorkspaceError),
}

/// Instance-owned isolated-workspace service state: the typed config plus the
/// lazily bound layer-stack + manager pair.
pub struct WorkspaceRuntime {
    config: IsolatedWorkspaceConfig,
    command: Arc<CommandOps>,
    mode_gate: Mutex<()>,
    state: Mutex<Option<BoundState>>,
}

impl WorkspaceRuntime {
    #[must_use]
    pub fn new(config: IsolatedWorkspaceConfig, command: Arc<CommandOps>) -> Self {
        Self {
            config,
            command,
            mode_gate: Mutex::new(()),
            state: Mutex::new(None),
        }
    }

    /// Open an isolated workspace for `caller_id` at `workspace_root`: resolve
    /// the LayerStack binding, bind (or rebind) the manager, acquire a snapshot
    /// lease, and enter. The lease is released again when `enter` fails.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceEnterError::ActiveCommands`] when the caller
    /// has live commands, [`IsolatedError::FeatureDisabled`] when
    /// isolation is disabled, and the manager's enter/setup errors otherwise.
    pub fn enter(
        &self,
        caller_id: &str,
        workspace_root: &Path,
    ) -> Result<IsolatedWorkspaceHandle, WorkspaceEnterError> {
        self.enter_with_report(caller_id, workspace_root)
            .map(|outcome| outcome.handle)
    }

    pub(crate) fn enter_with_report(
        &self,
        caller_id: &str,
        workspace_root: &Path,
    ) -> Result<WorkspaceEnterOutcome, WorkspaceEnterError> {
        let _mode_guard = self.lock_mode_gate();
        self.reject_disabled()?;
        self.reject_active_commands(caller_id)?;
        let resolved = self.resolve_workspace_root(workspace_root)?;
        self.enter_resolved_locked(caller_id, &resolved)
    }

    pub(crate) fn enter_with_report_legacy_layer_stack_root(
        &self,
        caller_id: &str,
        layer_stack_root: &Path,
    ) -> Result<WorkspaceEnterOutcome, WorkspaceEnterError> {
        let _mode_guard = self.lock_mode_gate();
        self.reject_disabled()?;
        self.reject_active_commands(caller_id)?;
        let resolved = self.resolve_legacy_layer_stack_root(layer_stack_root)?;
        self.enter_resolved_locked(caller_id, &resolved)
    }

    fn enter_resolved_locked(
        &self,
        caller_id: &str,
        resolved: &ResolvedWorkspaceRoot,
    ) -> Result<WorkspaceEnterOutcome, WorkspaceEnterError> {
        let recovery = self.ensure_state(resolved)?;
        let mut guard = self.lock_state_cell();
        let state = guard.as_mut().ok_or(IsolatedError::FeatureDisabled)?;
        let (snapshot, snapshot_normalization) = state.acquire_snapshot(
            caller_id,
            self.command.commit_options().auto_squash_max_depth,
        )?;
        let lease_id = snapshot.lease_id.clone();
        match state.manager.enter(caller_id, snapshot) {
            Ok(handle) => Ok(WorkspaceEnterOutcome {
                handle,
                recovery,
                snapshot_normalization,
            }),
            Err(error) => {
                let lease_release = state.release_lease(&lease_id);
                Err(WorkspaceEnterError::EnterFailed {
                    source: error,
                    lease_release,
                })
            }
        }
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
    pub fn exit(
        &self,
        caller_id: &str,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedError> {
        let _mode_guard = self.lock_mode_gate();
        self.exit_locked(caller_id, grace_s)
    }

    /// The caller's open handle, or `Ok(None)` when no workspace is open.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::FeatureDisabled`] when isolation is disabled.
    pub fn status(
        &self,
        caller_id: &str,
    ) -> Result<Option<IsolatedWorkspaceHandle>, IsolatedError> {
        self.with_state(|state| Ok(state.manager.get_handle(caller_id)))
    }

    pub(crate) fn resolve_workspace_root(
        &self,
        workspace_root: &Path,
    ) -> Result<ResolvedWorkspaceRoot, WorkspaceError> {
        let workspace_root = validate_caller_workspace_root(workspace_root)?;
        let mut matches = self.discover_workspace_root_bindings(&workspace_root)?;
        if let Some(resolved) = self.resolve_bound_workspace_root(&workspace_root)? {
            matches
                .entry(resolved.layer_stack_root.clone())
                .or_insert(resolved);
        }
        match matches.len() {
            0 => Err(WorkspaceError::InvalidRequest {
                field: "workspace_root",
                message: format!(
                    "no LayerStack workspace binding found for {}",
                    workspace_root.display()
                ),
            }),
            1 => Ok(matches
                .into_values()
                .next()
                .expect("one match should be present")),
            _ => {
                let roots = matches
                    .keys()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(WorkspaceError::InvalidRequest {
                    field: "workspace_root",
                    message: format!(
                        "ambiguous workspace_root {}; matching layer stack roots: {roots}",
                        workspace_root.display()
                    ),
                })
            }
        }
    }

    pub(crate) fn resolve_legacy_layer_stack_root(
        &self,
        layer_stack_root: &Path,
    ) -> Result<ResolvedWorkspaceRoot, WorkspaceError> {
        let requested_root = validate_legacy_layer_stack_root(layer_stack_root)?;
        let binding = read_workspace_binding(&requested_root)
            .map_err(workspace_setup_error)?
            .ok_or_else(|| WorkspaceError::InvalidRequest {
                field: "layer_stack_root",
                message: format!(
                    "workspace binding is missing: {}",
                    requested_root.join(WORKSPACE_BINDING_FILE).display()
                ),
            })?;
        resolved_workspace_binding_at(&requested_root, binding)
    }

    pub(crate) fn create_host_workspace_for_legacy_layer_stack_root_locked(
        &self,
        caller_id: &str,
        invocation_id: &str,
        layer_stack_root: &Path,
    ) -> Result<HostWorkspaceLifecycle, WorkspaceError> {
        let resolved = self.resolve_legacy_layer_stack_root(layer_stack_root)?;
        self.create_host_workspace(caller_id, invocation_id, &resolved, || {
            EphemeralWorkspace::create_runtime_overlay("sandbox-overlay", invocation_id)
                .map_err(host_workspace_setup_error)
        })
    }

    pub(crate) fn route_command_context<'a>(
        &'a self,
        caller_id: &str,
        invocation_id: &str,
        layer_stack_root: Option<PathBuf>,
    ) -> Result<WorkspaceCommandRouteContext<'a>, WorkspaceError> {
        self.route_command_context_with_host_creator(
            caller_id,
            invocation_id,
            layer_stack_root,
            |runtime, caller_id, invocation_id, layer_stack_root| {
                runtime.create_host_workspace_for_legacy_layer_stack_root_locked(
                    caller_id,
                    invocation_id,
                    layer_stack_root,
                )
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn route_command_context_with_scratch_root_for_test<'a>(
        &'a self,
        caller_id: &str,
        invocation_id: &str,
        layer_stack_root: Option<PathBuf>,
        scratch_root: &'a Path,
    ) -> Result<WorkspaceCommandRouteContext<'a>, WorkspaceError> {
        self.route_command_context_with_host_creator(
            caller_id,
            invocation_id,
            layer_stack_root,
            move |runtime, caller_id, invocation_id, layer_stack_root| {
                let resolved = runtime.resolve_legacy_layer_stack_root(layer_stack_root)?;
                runtime.create_host_workspace(caller_id, invocation_id, &resolved, || {
                    EphemeralWorkspace::create(scratch_root, "sandbox-overlay", invocation_id)
                        .map_err(host_workspace_setup_error)
                })
            },
        )
    }

    fn route_command_context_with_host_creator<'a>(
        &'a self,
        caller_id: &str,
        invocation_id: &str,
        layer_stack_root: Option<PathBuf>,
        create_host_workspace: impl FnOnce(
            &Self,
            &str,
            &str,
            &Path,
        ) -> Result<HostWorkspaceLifecycle, WorkspaceError>,
    ) -> Result<WorkspaceCommandRouteContext<'a>, WorkspaceError> {
        let mode_guard = self.lock_mode_gate();
        if let Some(binding) = self.command_binding_for(caller_id) {
            return Ok(WorkspaceCommandRouteContext {
                route: WorkspaceCommandRoute::Isolated { binding },
                trace: WorkspaceRouteTraceFacts {
                    kind: "isolated_workspace",
                    reason: "caller_has_open_isolated_workspace",
                    layer_stack_root: None,
                },
                _mode_guard: mode_guard,
            });
        }

        let requested_root = layer_stack_root.ok_or_else(missing_layer_stack_root_error)?;
        let workspace = create_host_workspace(self, caller_id, invocation_id, &requested_root)?;
        Ok(WorkspaceCommandRouteContext {
            route: WorkspaceCommandRoute::Host {
                caller_id: caller_id.to_owned(),
                workspace,
            },
            trace: WorkspaceRouteTraceFacts {
                kind: "ephemeral_workspace",
                reason: "no_isolated_workspace_for_caller",
                layer_stack_root: Some(requested_root),
            },
            _mode_guard: mode_guard,
        })
    }

    pub(crate) fn route_file_context(
        &self,
        caller_id: &str,
        layer_stack_root: Option<&Path>,
    ) -> Result<WorkspaceFileRouteContext, WorkspaceError> {
        if let Some(binding) = self.command_binding_for(caller_id) {
            return Ok(WorkspaceFileRouteContext::Isolated { binding });
        }
        Self::direct_file_context(layer_stack_root)
    }

    pub(crate) fn direct_file_context(
        layer_stack_root: Option<&Path>,
    ) -> Result<WorkspaceFileRouteContext, WorkspaceError> {
        let layer_stack_root = layer_stack_root
            .ok_or_else(missing_layer_stack_root_error)?
            .to_path_buf();
        Ok(WorkspaceFileRouteContext::Direct { layer_stack_root })
    }

    pub(crate) fn complete_file_route(&self, route: &WorkspaceFileRouteContext) {
        if let WorkspaceFileRouteContext::Isolated { binding } = route {
            self.touch(&binding.caller_id);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn capture_changes(
        &self,
        handle: &UnifiedWorkspaceHandle,
        request: CaptureChangesRequest,
    ) -> Result<CaptureChangesResult, WorkspaceError> {
        let _mode_guard = self.lock_mode_gate();
        self.reject_capture_active_commands(&handle.owner)?;
        match handle.network {
            NetworkMode::Isolated => {
                let binding = self.command_binding_for(&handle.owner.0).ok_or_else(|| {
                    WorkspaceError::NotOpen {
                        owner: handle.owner.clone(),
                    }
                })?;
                ensure_isolated_capture_handle_matches(handle, &binding)?;
                let result = capture_upperdir_result(
                    &binding.upperdir,
                    WorkspaceId(binding.workspace_handle_id.clone()),
                    base_revision_from_isolated_binding(&binding),
                    request,
                )?;
                self.touch(&binding.caller_id);
                Ok(result)
            }
            NetworkMode::Host => Err(WorkspaceError::InvalidRequest {
                field: "handle",
                message: "host capture requires the runtime-owned Host workspace lifecycle"
                    .to_owned(),
            }),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn capture_host_workspace_changes(
        &self,
        host: &HostWorkspaceLifecycle,
        request: CaptureChangesRequest,
    ) -> Result<CaptureChangesResult, WorkspaceError> {
        let _mode_guard = self.lock_mode_gate();
        let owner = CallerId(host.caller_id.clone());
        self.reject_capture_active_commands(&owner)?;
        capture_upperdir_result(
            &host.workspace.dirs().upperdir,
            host.workspace_id.clone(),
            base_revision_from_host(host),
            request,
        )
    }

    #[cfg(test)]
    pub(crate) fn create_host_workspace_for_legacy_layer_stack_root_with_scratch_root_for_test(
        &self,
        caller_id: &str,
        invocation_id: &str,
        layer_stack_root: &Path,
        scratch_root: &Path,
    ) -> Result<HostWorkspaceLifecycle, WorkspaceError> {
        let _mode_guard = self.lock_mode_gate();
        let resolved = self.resolve_legacy_layer_stack_root(layer_stack_root)?;
        self.create_host_workspace(caller_id, invocation_id, &resolved, || {
            EphemeralWorkspace::create(scratch_root, "sandbox-overlay", invocation_id)
                .map_err(host_workspace_setup_error)
        })
    }

    fn create_host_workspace(
        &self,
        caller_id: &str,
        invocation_id: &str,
        resolved: &ResolvedWorkspaceRoot,
        create_workspace: impl FnOnce() -> Result<EphemeralWorkspace, WorkspaceError>,
    ) -> Result<HostWorkspaceLifecycle, WorkspaceError> {
        let (leased_base, snapshot_normalization) =
            self.acquire_host_base_revision(resolved, caller_id, invocation_id)?;
        let lease = LeaseReleaseHandle::new(
            resolved.layer_stack_root.clone(),
            leased_base.lease_id.clone(),
        );
        let workspace = match create_workspace() {
            Ok(workspace) => workspace,
            Err(error) => {
                let release = lease.release();
                return Err(host_create_failed_error(
                    error,
                    &leased_base.lease_id,
                    release,
                ));
            }
        };
        Ok(HostWorkspaceLifecycle {
            caller_id: caller_id.to_owned(),
            workspace_id: WorkspaceId(format!("host-{invocation_id}")),
            layer_stack_root: resolved.layer_stack_root.clone(),
            workspace_root: resolved.workspace_root.clone(),
            leased_base,
            snapshot_normalization,
            workspace,
            lease,
        })
    }

    fn acquire_host_base_revision(
        &self,
        resolved: &ResolvedWorkspaceRoot,
        caller_id: &str,
        invocation_id: &str,
    ) -> Result<(LeasedBaseRevision, SnapshotNormalization), WorkspaceError> {
        let request_id = format!("command:{caller_id}:{invocation_id}");
        let command_snapshot = layerstack::service::acquire_bounded_snapshot_for_command(
            &resolved.layer_stack_root,
            &request_id,
            self.command.commit_options().auto_squash_max_depth,
        )
        .map_err(|error| WorkspaceError::SnapshotAcquire {
            source: error.to_string(),
        })?;
        let snapshot = command_snapshot.snapshot;
        Ok((
            LeasedBaseRevision {
                lease_id: snapshot.lease_id,
                version: snapshot.manifest_version,
                root_hash: snapshot.root_hash,
                layer_paths: snapshot.layer_paths,
            },
            command_snapshot.normalization,
        ))
    }

    fn resolve_bound_workspace_root(
        &self,
        workspace_root: &Path,
    ) -> Result<Option<ResolvedWorkspaceRoot>, WorkspaceError> {
        let Some(layer_stack_root) = self
            .lock_state_cell()
            .as_ref()
            .map(|state| state.layer_stack_root.clone())
        else {
            return Ok(None);
        };
        let Some(binding) =
            read_workspace_binding(&layer_stack_root).map_err(workspace_setup_error)?
        else {
            return Ok(None);
        };
        if !paths_match(Path::new(&binding.workspace_root), workspace_root) {
            return Ok(None);
        }
        resolved_workspace_binding_at(&layer_stack_root, binding).map(Some)
    }

    fn discover_workspace_root_bindings(
        &self,
        workspace_root: &Path,
    ) -> Result<BTreeMap<PathBuf, ResolvedWorkspaceRoot>, WorkspaceError> {
        let mut matches = BTreeMap::new();
        for root in self.workspace_binding_search_roots(workspace_root) {
            collect_workspace_binding_matches(
                &root,
                workspace_root,
                WORKSPACE_BINDING_SEARCH_DEPTH,
                &mut matches,
            )?;
        }
        Ok(matches)
    }

    fn workspace_binding_search_roots(&self, workspace_root: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        push_search_root(&mut roots, self.config.scratch_root.as_path());
        if let Some(parent) = self.config.scratch_root.parent() {
            push_search_root(&mut roots, parent);
        }
        if let Some(parent) = workspace_root.parent() {
            push_search_root(&mut roots, parent);
        }
        roots
    }

    pub(crate) fn compact_remount_open_workspace_for_test(
        &self,
        caller_id: &str,
        workspace_root: &Path,
        probe: RemountProbe,
        test_force_block_reason: Option<IsolationTestRemountFault>,
    ) -> Result<WorkspaceRemountCompactionAttempt, IsolatedError> {
        let _mode_guard = self.lock_mode_gate();
        let resolved = self
            .resolve_workspace_root(workspace_root)
            .map_err(workspace_error_as_isolated_error)?;
        self.compact_remount_open_workspace_for_test_resolved_locked(
            caller_id,
            &resolved,
            probe,
            test_force_block_reason,
        )
    }

    pub(crate) fn compact_remount_open_workspace_for_test_legacy_layer_stack_root(
        &self,
        caller_id: &str,
        layer_stack_root: &Path,
        probe: RemountProbe,
        test_force_block_reason: Option<IsolationTestRemountFault>,
    ) -> Result<WorkspaceRemountCompactionAttempt, IsolatedError> {
        let _mode_guard = self.lock_mode_gate();
        let resolved = self
            .resolve_legacy_layer_stack_root(layer_stack_root)
            .map_err(workspace_error_as_isolated_error)?;
        self.compact_remount_open_workspace_for_test_resolved_locked(
            caller_id,
            &resolved,
            probe,
            test_force_block_reason,
        )
    }

    fn compact_remount_open_workspace_for_test_resolved_locked(
        &self,
        caller_id: &str,
        resolved: &ResolvedWorkspaceRoot,
        probe: RemountProbe,
        test_force_block_reason: Option<IsolationTestRemountFault>,
    ) -> Result<WorkspaceRemountCompactionAttempt, IsolatedError> {
        self.ensure_state(resolved)?;
        self.with_state(|state| state.manager.mark_remount_pending(caller_id))?;
        let result = self.compact_remount_open_workspace_marked_pending(
            caller_id,
            probe,
            test_force_block_reason,
        );
        self.with_state(|state| state.manager.clear_remount_pending(caller_id))?;
        result
    }

    fn compact_remount_open_workspace_marked_pending(
        &self,
        caller_id: &str,
        probe: RemountProbe,
        test_force_block_reason: Option<IsolationTestRemountFault>,
    ) -> Result<WorkspaceRemountCompactionAttempt, IsolatedError> {
        let mut quiesce = self.command.begin_live_remount_for_caller(caller_id);
        if let Some(fault) = test_force_block_reason {
            let inspection = forced_remount_block_inspection(quiesce.finish(), fault);
            return self
                .with_state(|state| state.blocked_remount_report_for_test(caller_id, inspection))
                .map(WorkspaceRemountCompactionAttempt::Blocked);
        }
        if quiesce.inspection().active_commands > 0 {
            if quiesce.inspection().can_live_remount() {
                let inspection = quiesce.inspection().clone();
                let result = self
                    .with_state(|state| {
                        state.compact_remount_open_workspace_for_test(
                            caller_id,
                            &probe,
                            Some(&inspection),
                        )
                    })
                    .map(|mut report| {
                        report.process_resumed = quiesce.resume();
                        WorkspaceRemountCompactionAttempt::Compacted(report)
                    });
                return result;
            }
            let inspection = quiesce.finish();
            return self
                .with_state(|state| state.blocked_remount_report_for_test(caller_id, inspection))
                .map(WorkspaceRemountCompactionAttempt::Blocked);
        }
        self.with_state(|state| {
            state.compact_remount_open_workspace_for_test(caller_id, &probe, None)
        })
        .map(WorkspaceRemountCompactionAttempt::Compacted)
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

    /// The command binding for `caller_id`'s open workspace, or `None`
    /// when the caller is not isolated (callers then route ephemerally).
    #[must_use]
    pub fn command_binding_for(&self, caller_id: &str) -> Option<IsolatedWorkspaceBinding> {
        if caller_id.is_empty() {
            return None;
        }
        let guard = self.lock_state_cell();
        let state = guard.as_ref()?;
        let handle = state.manager.get_handle(caller_id)?;
        Some(command_binding_from(&state.layer_stack_root, handle))
    }

    pub(crate) fn lock_mode_gate(&self) -> MutexGuard<'_, ()> {
        self.mode_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Cancel every workspace run owned by `caller_id`: discard its commands,
    /// then exit its isolated workspace if open. The order matters: commands
    /// are cancelled before the isolated namespace/lease teardown.
    pub fn cancel_runs_for_caller(&self, caller_id: &str, grace_s: Option<f64>) -> CallerCancel {
        let _mode_guard = self.lock_mode_gate();
        let cancelled_commands = self.command.cleanup_caller(caller_id, grace_s);
        let isolated = self.exit_locked(caller_id, grace_s);
        CallerCancel {
            cancelled_commands,
            isolated,
        }
    }

    /// Cancel every workspace run in the sandbox: discard all commands, exit
    /// every isolated caller, then reap orphaned namespace/
    /// cgroup/scratch resources. Returns the per-substrate counts as
    /// `(cancelled_commands, isolated_callers_exited)`.
    pub fn cancel_all_runs(&self, grace_s: Option<f64>) -> (usize, usize) {
        let _mode_guard = self.lock_mode_gate();
        let cancelled_commands = self.command.cancel_all(grace_s);
        let isolated_exited = self.exit_all_and_reap(grace_s);
        (cancelled_commands, isolated_exited)
    }

    /// Exit every open isolated workspace and reap orphaned resources (the
    /// whole-sandbox cancel cleanup). Returns the number of callers exited.
    fn exit_all_and_reap(&self, grace_s: Option<f64>) -> usize {
        let mut guard = self.lock_state_cell();
        let Some(state) = guard.as_mut() else {
            return 0;
        };
        let callers = state.manager.list_open_callers();
        for caller in &callers {
            let _ = state.exit_caller(caller, grace_s);
        }
        let _ = state.manager.reap_orphan_resources();
        callers.len()
    }

    fn exit_locked(
        &self,
        caller_id: &str,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedError> {
        self.with_state(|state| state.exit_caller(caller_id, grace_s))
    }

    /// Evict idle isolated workspaces past their TTL, releasing their leases.
    /// Callers that still own a live command are protected.
    pub(crate) fn evict_idle_workspaces_report(&self) -> IdleWorkspaceEvictionReport {
        let _mode_guard = self.lock_mode_gate();
        let mut guard = self.lock_state_cell();
        let Some(state) = guard.as_mut() else {
            return IdleWorkspaceEvictionReport::default();
        };
        // The command registry is the authority for caller liveness
        // (lock order: workspace state -> command registry).
        let active_callers = state
            .manager
            .list_open_callers()
            .into_iter()
            .filter(|caller| self.command.count_by_caller(Some(caller)) > 0)
            .collect::<HashSet<_>>();
        let evicted = state.manager.evict_idle_workspaces(&active_callers);
        let mut report = IdleWorkspaceEvictionReport {
            evicted: Vec::with_capacity(evicted.len()),
        };
        for outcome in evicted {
            let lease_release = state.release_lease(&outcome.lease_id);
            let active_leases_after = state.stack.active_lease_count();
            report.evicted.push(IdleWorkspaceEviction {
                caller_id: outcome.caller_id,
                workspace_handle_id: outcome.workspace_id.0,
                lease_id: outcome.lease_id,
                evicted_upperdir_bytes: outcome.evicted_upperdir_bytes,
                lifetime_s: outcome.lifetime_s,
                total_ms: outcome.total_ms,
                lease_release,
                active_leases_after,
            });
        }
        report
    }

    /// Exit every caller, drop the bound state, and rewrite the persisted
    /// manager file (backs `sandbox.isolation.test_reset`). Returns the caller
    /// ids that were exited.
    pub fn test_reset(&self) -> Vec<String> {
        self.test_reset_report().exited_callers
    }

    /// Exit every caller, drop the bound state, rewrite the persisted manager
    /// file, and retain recovery facts for request-side tracing.
    pub(crate) fn test_reset_report(&self) -> WorkspaceRecoveryReport {
        let _mode_guard = self.lock_mode_gate();
        let manager_json_error = manager_json_error(&self.config.scratch_root);
        let mut orphan_cleanup_error = None;
        let exited_callers = {
            let mut guard = self.lock_state_cell();
            let exited_callers = if let Some(state) = guard.as_mut() {
                let callers = state.manager.list_open_callers();
                for caller_id in &callers {
                    let _ = state.exit_caller(caller_id, Some(0.0));
                }
                orphan_cleanup_error = state.manager.reap_orphan_resources();
                callers
            } else {
                Vec::new()
            };
            *guard = None;
            exited_callers
        };
        self.reset_test_manager_file();
        WorkspaceRecoveryReport {
            attempted: true,
            exited_callers,
            manager_json_error,
            orphan_cleanup_error,
        }
    }

    /// Bind (or rebind) the isolated manager to `root`, initializing caps from
    /// the resolved workspace binding and releasing leases orphaned by a prior
    /// daemon.
    fn ensure_state(
        &self,
        root: &ResolvedWorkspaceRoot,
    ) -> Result<WorkspaceRecoveryReport, IsolatedError> {
        let layer_stack_root = root.layer_stack_root.clone();
        let workspace_root = root.workspace_root.clone();
        let mut recovery = WorkspaceRecoveryReport::default();
        {
            let mut guard = self.lock_state_cell();
            if let Some(state) = guard.as_mut() {
                if state.layer_stack_root != layer_stack_root
                    || state.workspace_root != workspace_root
                {
                    // Block rebinding to a new root only while an isolated workspace
                    // is open: those handles pin leases/namespaces on the old root.
                    // (Isolated commands belong to an open caller, so this
                    // already covers them; ephemeral commands are unrelated
                    // to the isolated manager's binding and must not block a rebind.)
                    let open_callers = state.manager.list_open_callers();
                    if !open_callers.is_empty() {
                        return Err(IsolatedError::SetupFailed {
                            step: format!(
                                "isolated workspace manager is bound to workspace_root {} and layer_stack_root {} with active callers",
                                state.workspace_root.display(),
                                state.layer_stack_root.display()
                            ),
                        });
                    }
                    recovery.attempted = true;
                    recovery.merge_orphan_cleanup_error(state.manager.reap_orphan_resources());
                    *guard = None;
                }
            }
            if guard.is_none() {
                let mut caps = resource_caps_from_config(&self.config);
                if !caps.enabled {
                    return Err(IsolatedError::FeatureDisabled);
                }
                caps.eos_workspace_root = root.workspace_root.to_string_lossy().into_owned();
                let mut stack = LayerStack::open(layer_stack_root.clone()).map_err(setup_error)?;
                let mut manager =
                    IsolatedManager::with_scratch_root(caps, self.config.scratch_root.clone());
                let cleanup = manager.initialize_report()?;
                recovery.attempted = true;
                recovery.merge_orphan_cleanup_error(cleanup.cleanup_error);
                for lease_id in cleanup.orphan_lease_ids {
                    if let Err(err) = stack.release_lease(&lease_id) {
                        recovery.merge_orphan_cleanup_error(Some(format!(
                            "release orphan lease {lease_id}: {err}"
                        )));
                    }
                }
                *guard = Some(BoundState {
                    workspace_root,
                    layer_stack_root,
                    stack,
                    manager,
                });
            }
        }
        Ok(recovery)
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

    fn reject_active_commands(&self, caller_id: &str) -> Result<(), WorkspaceEnterError> {
        let active_commands = self.command.count_by_caller(Some(caller_id));
        if active_commands > 0 {
            return Err(WorkspaceEnterError::ActiveCommands { active_commands });
        }
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn reject_capture_active_commands(&self, owner: &CallerId) -> Result<(), WorkspaceError> {
        let active_commands = self.command.count_by_caller(Some(owner.0.as_str()));
        capture_active_command_rejection(owner, active_commands)
    }

    fn reject_disabled(&self) -> Result<(), WorkspaceEnterError> {
        if self.config.enabled {
            Ok(())
        } else {
            Err(WorkspaceEnterError::Isolated(
                IsolatedError::FeatureDisabled,
            ))
        }
    }

    fn reset_test_manager_file(&self) {
        let scratch_root = &self.config.scratch_root;
        let _ = std::fs::remove_dir_all(scratch_root);
        if std::fs::create_dir_all(scratch_root).is_err() {
            return;
        }
        let _ = std::fs::write(
            scratch_root.join("manager.json"),
            br#"{"schema_version":1,"handles":[]}"#,
        );
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn capture_active_command_rejection(
    owner: &CallerId,
    active_commands: usize,
) -> Result<(), WorkspaceError> {
    if active_commands > 0 {
        return Err(WorkspaceError::ActiveCommands {
            owner: owner.clone(),
            active_commands,
        });
    }
    Ok(())
}

fn command_binding_from(
    layer_stack_root: &Path,
    handle: IsolatedWorkspaceHandle,
) -> IsolatedWorkspaceBinding {
    IsolatedWorkspaceBinding {
        caller_id: handle.caller_id,
        workspace_handle_id: handle.workspace_id.0,
        layer_stack_root: layer_stack_root.to_path_buf(),
        manifest_version: handle.manifest_version,
        manifest_root_hash: handle.manifest_root_hash,
        workspace_root: PathBuf::from(handle.workspace_root),
        scratch_dir: handle.dirs.run_dir,
        upperdir: handle.dirs.upperdir,
        workdir: handle.dirs.workdir,
        layer_paths: handle.layer_paths,
        ns_fds: handle.ns_fds,
        cgroup_path: handle.cgroup_path,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn ensure_isolated_capture_handle_matches(
    handle: &UnifiedWorkspaceHandle,
    binding: &IsolatedWorkspaceBinding,
) -> Result<(), WorkspaceError> {
    if binding.workspace_handle_id != handle.id.0 {
        return Err(WorkspaceError::NotOpen {
            owner: handle.owner.clone(),
        });
    }
    if !paths_match(&binding.workspace_root, &handle.workspace_root) {
        return Err(WorkspaceError::InvalidRequest {
            field: "workspace_root",
            message: format!(
                "workspace_root does not match open workspace handle: expected {}, got {}",
                binding.workspace_root.display(),
                handle.workspace_root.display()
            ),
        });
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn capture_upperdir_result(
    upperdir: &Path,
    workspace_id: WorkspaceId,
    base_revision: BaseRevision,
    request: CaptureChangesRequest,
) -> Result<CaptureChangesResult, WorkspaceError> {
    let captured = capture_upperdir_with_payloads(upperdir, request.materialize_payloads).map_err(
        |error| WorkspaceError::Capture {
            message: error.to_string(),
        },
    )?;
    let changed_path_kinds = captured
        .changes
        .iter()
        .map(|change| {
            (
                change.path().as_str().to_owned(),
                ChangedPathKind::from(change),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let changed_paths = changed_path_kinds.keys().cloned().collect();
    let protected_drops = captured
        .protected_drops
        .iter()
        .map(ProtectedPathDrop::from)
        .collect();
    Ok(CaptureChangesResult {
        workspace_id,
        base_revision,
        changed_paths,
        changed_path_kinds,
        protected_drops,
        stats: request.include_stats.then_some(captured.stats),
    })
}

#[cfg_attr(not(test), allow(dead_code))]
fn base_revision_from_host(host: &HostWorkspaceLifecycle) -> BaseRevision {
    BaseRevision {
        version: host.leased_base.version,
        root_hash: host.leased_base.root_hash.clone(),
        layer_count: host.leased_base.layer_paths.len(),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn base_revision_from_isolated_binding(binding: &IsolatedWorkspaceBinding) -> BaseRevision {
    BaseRevision {
        version: binding.manifest_version,
        root_hash: binding.manifest_root_hash.clone(),
        layer_count: binding.layer_paths.len(),
    }
}

fn validate_caller_workspace_root(workspace_root: &Path) -> Result<PathBuf, WorkspaceError> {
    validate_absolute_root("workspace_root", workspace_root)
}

fn validate_legacy_layer_stack_root(layer_stack_root: &Path) -> Result<PathBuf, WorkspaceError> {
    validate_absolute_root("layer_stack_root", layer_stack_root)
}

fn validate_absolute_root(field: &'static str, root: &Path) -> Result<PathBuf, WorkspaceError> {
    if root.as_os_str().is_empty() {
        return Err(WorkspaceError::InvalidRequest {
            field,
            message: "path is required".to_owned(),
        });
    }
    if !root.is_absolute() {
        return Err(WorkspaceError::InvalidRequest {
            field,
            message: format!("path must be absolute: {}", root.display()),
        });
    }
    Ok(normalized_root(root))
}

fn resolved_workspace_binding(
    binding: WorkspaceBinding,
) -> Result<ResolvedWorkspaceRoot, WorkspaceError> {
    let workspace_root = validate_binding_root("workspace_root", &binding.workspace_root)?;
    let layer_stack_root = validate_binding_root("layer_stack_root", &binding.layer_stack_root)?;
    if paths_match(&workspace_root, &layer_stack_root)
        || layer_stack_root.starts_with(&workspace_root)
    {
        return Err(WorkspaceError::InvalidRequest {
            field: "layer_stack_root",
            message: format!(
                "layer_stack_root must be outside workspace_root: {} is inside {}",
                layer_stack_root.display(),
                workspace_root.display()
            ),
        });
    }
    Ok(ResolvedWorkspaceRoot {
        workspace_root,
        layer_stack_root,
        binding,
    })
}

fn resolved_workspace_binding_at(
    binding_path_root: &Path,
    binding: WorkspaceBinding,
) -> Result<ResolvedWorkspaceRoot, WorkspaceError> {
    let resolved = resolved_workspace_binding(binding)?;
    if !paths_match(binding_path_root, &resolved.layer_stack_root) {
        return Err(WorkspaceError::InvalidRequest {
            field: "layer_stack_root",
            message: format!(
                "workspace binding at {} points at different layer_stack_root {}",
                binding_path_root.display(),
                resolved.layer_stack_root.display()
            ),
        });
    }
    Ok(resolved)
}

fn validate_binding_root(field: &'static str, raw_root: &str) -> Result<PathBuf, WorkspaceError> {
    let root = PathBuf::from(raw_root.trim());
    validate_absolute_root(field, &root)
}

fn collect_workspace_binding_matches(
    root: &Path,
    workspace_root: &Path,
    depth: usize,
    matches: &mut BTreeMap<PathBuf, ResolvedWorkspaceRoot>,
) -> Result<(), WorkspaceError> {
    if paths_match(root, workspace_root) || !root.is_dir() {
        return Ok(());
    }
    if root.join(WORKSPACE_BINDING_FILE).is_file() {
        if let Some(binding) = read_workspace_binding(root).map_err(workspace_setup_error)? {
            if paths_match(Path::new(&binding.workspace_root), workspace_root) {
                let resolved = resolved_workspace_binding_at(root, binding)?;
                matches
                    .entry(resolved.layer_stack_root.clone())
                    .or_insert(resolved);
            }
        }
    }
    if depth == 0 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => {
            entries
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| WorkspaceError::Setup {
                    step: format!(
                        "read workspace binding search root {}: {err}",
                        root.display()
                    ),
                })?
        }
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(WorkspaceError::Setup {
                step: format!(
                    "read workspace binding search root {}: {err}",
                    root.display()
                ),
            });
        }
    };
    let mut children = entries;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let file_type = child.file_type().map_err(|err| WorkspaceError::Setup {
            step: format!(
                "read workspace binding search entry {}: {err}",
                child.path().display()
            ),
        })?;
        if file_type.is_dir() {
            collect_workspace_binding_matches(
                &child.path(),
                workspace_root,
                depth.saturating_sub(1),
                matches,
            )?;
        }
    }
    Ok(())
}

fn push_search_root(roots: &mut Vec<PathBuf>, root: &Path) {
    let root = normalized_root(root);
    if is_filesystem_root(&root) || roots.iter().any(|existing| paths_match(existing, &root)) {
        return;
    }
    roots.push(root);
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn paths_match(left: &Path, right: &Path) -> bool {
    normalized_root(left) == normalized_root(right)
}

fn workspace_setup_error(error: layerstack::LayerStackError) -> WorkspaceError {
    WorkspaceError::Setup {
        step: error.to_string(),
    }
}

fn host_workspace_setup_error(error: workspace::EphemeralWorkspaceError) -> WorkspaceError {
    WorkspaceError::Setup {
        step: error.to_string(),
    }
}

fn host_create_failed_error(
    error: WorkspaceError,
    lease_id: &str,
    release: LayerStackLeaseReleaseReport,
) -> WorkspaceError {
    let mut step = format!("host workspace create failed after lease acquisition: {error}");
    match (release.released, release.error) {
        (Some(true), None) => {
            step.push_str(&format!("; lease {lease_id} released"));
        }
        (Some(false), None) => {
            step.push_str(&format!("; lease {lease_id} was already released"));
        }
        (_, Some(release_error)) => {
            step.push_str(&format!(
                "; lease {lease_id} release failed: {release_error}"
            ));
        }
        (None, None) => {}
    }
    WorkspaceError::Setup { step }
}

fn missing_layer_stack_root_error() -> WorkspaceError {
    WorkspaceError::InvalidRequest {
        field: "layer_stack_root",
        message: "layer_stack_root is required".to_owned(),
    }
}

fn workspace_error_as_isolated_error(error: WorkspaceError) -> IsolatedError {
    match error {
        error @ WorkspaceError::InvalidRequest { .. } => {
            IsolatedError::InvalidArgument(error.to_string())
        }
        error => IsolatedError::SetupFailed {
            step: error.to_string(),
        },
    }
}

fn normalized_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn manager_json_error(scratch_root: &Path) -> Option<String> {
    let path = scratch_root.join("manager.json");
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return None,
        Err(err) => return Some(format!("manager_json_read: {err}")),
    };
    let payload = match serde_json::from_slice::<Value>(&raw) {
        Ok(payload) => payload,
        Err(err) => return Some(format!("manager_json_parse: {err}")),
    };
    let Some(schema_version) = payload.get("schema_version").and_then(Value::as_u64) else {
        return Some("manager_json_schema: missing schema_version".to_owned());
    };
    if schema_version != PERSISTED_HANDLES_SCHEMA_VERSION {
        return Some(format!(
            "manager_json_schema: expected schema_version {PERSISTED_HANDLES_SCHEMA_VERSION}, got {schema_version}"
        ));
    }
    if !payload.get("handles").is_some_and(Value::is_array) {
        return Some("manager_json_schema: handles must be an array".to_owned());
    }
    None
}

fn best_effort_file_bytes(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| best_effort_file_bytes(&entry.path()))
            .sum()
    })
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

#[cfg(test)]
#[path = "../../tests/unit/workspace_runtime.rs"]
mod tests;

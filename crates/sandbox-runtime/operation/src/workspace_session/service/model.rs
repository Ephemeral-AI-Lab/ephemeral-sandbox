use std::collections::BTreeSet;
use std::path::PathBuf;

use sandbox_runtime_mpla_poc::{
    AllocationHandle, AllocationId, CanonicalRootPair, ExactProjectionReceipt,
    ExternalStationaryPublicationReceipt, MutableLease, OperationId, PairedRefValue,
    PreparedExternalSession, RunId, SemanticBuildReceipt,
};
use sandbox_runtime_namespace_execution::NamespaceExecutionId;

use crate::layerstack::LayerStackRevision;
use crate::workspace_crate::{
    DestroyWorkspaceResult, ExecutionScratchRoute, NetworkProfile, WorkspaceHandle,
    WorkspaceSessionId,
};

/// What happens when a command completion empties the session's command
/// ledger. Fixed at creation; sessions created through the CLI are always
/// `NoOp`, `PublishThenDestroy` is set only by `exec_command`'s implicit
/// create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizePolicy {
    PublishThenDestroy,
    NoOp,
}

impl FinalizePolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PublishThenDestroy => "publish_then_destroy",
            Self::NoOp => "no_op",
        }
    }
}

/// Operation-layer session create request. Maps down to the policy-free
/// workspace-crate `CreateWorkspaceRequest`; the finalize policy stays in this
/// crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateSessionRequest {
    pub network: NetworkProfile,
    pub finalize_policy: FinalizePolicy,
}

/// Publish outcome of a finalize run, surfaced on the completing command's
/// terminal response through a once-set slot stored at attach (§2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeOutcome {
    pub publish_reject_class: Option<&'static str>,
    pub finalization_failure_class: Option<&'static str>,
    pub finalization_attempts: Option<usize>,
}

impl FinalizeOutcome {
    pub(crate) const fn publish_rejected(class: &'static str) -> Self {
        Self {
            publish_reject_class: Some(class),
            finalization_failure_class: None,
            finalization_attempts: None,
        }
    }

    pub(crate) const fn finalization_failed(class: &'static str, attempts: usize) -> Self {
        Self {
            publish_reject_class: None,
            finalization_failure_class: Some(class),
            finalization_attempts: Some(attempts),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSessionHandler {
    pub workspace_session_id: WorkspaceSessionId,
    pub handle: WorkspaceHandle,
    pub cgroup_path: Option<PathBuf>,
    pub(crate) execution_scratch_route: ExecutionScratchRoute,
}

impl WorkspaceSessionHandler {
    #[doc(hidden)]
    #[must_use]
    pub const fn execution_scratch_route(&self) -> ExecutionScratchRoute {
        self.execution_scratch_route
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFailureStage {
    Capture,
    Publish,
}

impl PublishFailureStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Publish => "publish",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSessionPublishDetails {
    pub no_op: bool,
    pub revision: LayerStackRevision,
    pub route_summary: sandbox_runtime_layerstack::PublishRouteSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishWorkspaceSessionResult {
    pub workspace_session_id: WorkspaceSessionId,
    pub publish: WorkspaceSessionPublishDetails,
    pub evicted_upperdir_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MplaLifecycleReceipt {
    pub operation_id: String,
    pub committed: bool,
    pub idempotent_replay: bool,
    pub selected_ref: String,
    pub service_elapsed_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivateMplaWorkspaceSessionResult {
    pub workspace_session_id: WorkspaceSessionId,
    pub fresh_allocation_id: AllocationId,
    pub run_id: String,
    pub branch: String,
    pub projection: ExactProjectionReceipt,
    pub lifecycle: MplaLifecycleReceipt,
    pub timings: MplaActivationTimings,
    pub service_elapsed_ns: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MplaActivationTimings {
    pub admission_elapsed_ns: u64,
    pub projection_elapsed_ns: u64,
    pub session_create_elapsed_ns: u64,
    pub session_identity_elapsed_ns: u64,
    pub allocation_create_elapsed_ns: u64,
    pub allocation_lease_elapsed_ns: u64,
    pub projection_metadata_elapsed_ns: u64,
    pub external_session_prepare_elapsed_ns: u64,
    pub durability_commit_elapsed_ns: u64,
    pub workspace_create_elapsed_ns: u64,
    pub launch_material_elapsed_ns: u64,
    pub cgroup_prepare_elapsed_ns: u64,
    pub session_register_elapsed_ns: u64,
    pub session_commit_elapsed_ns: u64,
    pub storage_mount_elapsed_ns: u64,
    pub outcome_persist_elapsed_ns: u64,
    pub response_elapsed_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkMplaWorkspaceSessionResult {
    pub run_id: String,
    pub source_branch: String,
    pub branch: String,
    pub lifecycle: MplaLifecycleReceipt,
    pub service_elapsed_ns: u64,
}

/// Receipt for attaching the fixed, immutable scorecard fixture to a fresh
/// sandbox-local MPLA run. The attach itself never creates an upper or copies
/// payload bytes; a later activation obtains the ordinary fresh upper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachMplaPreparedFixtureResult {
    pub run_id: String,
    pub fixture_profile: String,
    pub attached_branches: Vec<String>,
    pub cached_allocation_count: u64,
    pub payload_bytes_copied: u64,
    pub service_elapsed_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackMplaWorkspaceSessionResult {
    pub workspace_session_id: WorkspaceSessionId,
    pub fresh_allocation_id: AllocationId,
    pub run_id: String,
    pub branch: String,
    pub target_branch: String,
    pub projection: ExactProjectionReceipt,
    pub lifecycle: MplaLifecycleReceipt,
    pub service_elapsed_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishMplaWorkspaceSessionResult {
    pub workspace_session_id: WorkspaceSessionId,
    pub run_id: String,
    pub branch: String,
    pub lifecycle: MplaLifecycleReceipt,
    pub affected_path_count: u64,
    pub roots: CanonicalRootPair,
    pub semantic: Option<SemanticBuildReceipt>,
    pub stationary: Option<ExternalStationaryPublicationReceipt>,
    pub affected_payload_bytes_read: u64,
    pub affected_input_bytes: u64,
    pub semantic_affected_record_count: Option<u64>,
    pub prior_node_bytes_read: u64,
    pub immutable_payload_bytes_read: u64,
    pub semantic_root_record_debug: Option<String>,
    pub destroyed: bool,
    pub evicted_upperdir_bytes: u64,
    pub pre_storage_elapsed_ns: u64,
    pub storage_sequence_elapsed_ns: u64,
    pub storage_helper_to_unmount_elapsed_ns: u64,
    pub storage_stable_callback_elapsed_ns: u64,
    pub storage_helper_cleanup_elapsed_ns: u64,
    pub storage_helper_input_encode_elapsed_ns: u64,
    pub storage_helper_launch_elapsed_ns: u64,
    pub storage_helper_cgroup_placement_elapsed_ns: u64,
    pub storage_helper_request_write_elapsed_ns: u64,
    pub storage_helper_response_wait_elapsed_ns: u64,
    pub storage_helper_unmount_response_decode_elapsed_ns: u64,
    pub storage_helper_cgroup_release_elapsed_ns: u64,
    pub storage_helper_input_decode_elapsed_ns: u64,
    pub storage_helper_validation_elapsed_ns: u64,
    pub storage_helper_process_preparation_elapsed_ns: u64,
    pub storage_quiesce_lifecycle_elapsed_ns: u64,
    pub storage_quiesce_operation_elapsed_ns: u64,
    pub storage_strict_unmount_lifecycle_elapsed_ns: u64,
    pub storage_strict_unmount_operation_elapsed_ns: u64,
    pub semantic_adoption_elapsed_ns: u64,
    pub stationary_adoption_elapsed_ns: u64,
    pub semantic_build_elapsed_ns: u64,
    pub ref_commit_elapsed_ns: u64,
    pub session_destroy_elapsed_ns: u64,
    pub outcome_persist_elapsed_ns: u64,
    pub matched_publication_span: Option<sandbox_runtime_mpla_poc::MonotonicSpan>,
    pub service_elapsed_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquashMplaBranchResult {
    pub run_id: String,
    pub branch: String,
    pub lifecycle: MplaLifecycleReceipt,
    pub service_elapsed_ns: u64,
}

/// Lifecycle phase of a session's finalization. `FinalizeFailed` and a session
/// stuck in `Finalizing` are destroyable through `guarded_destroy` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizationState {
    Active,
    Finalizing,
    FinalizeFailed,
}

/// The only storage state in which an MPLA-backed workspace may admit an
/// ordinary workload.  Storage actions advance this state while holding the
/// same per-session admission gate used by commands and destruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MplaStoragePhase {
    Prepared,
    Mounted,
    Quiesced,
    Unmounted,
    Cleaned,
}

impl MplaStoragePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Mounted => "mounted",
            Self::Quiesced => "quiesced",
            Self::Unmounted => "unmounted",
            Self::Cleaned => "cleaned",
        }
    }
}

/// Server-owned binding for one externally mounted MPLA workspace.  It is
/// intentionally private to the operation layer: callers submit an exact
/// typed request, but never choose its roots, allocation, lease, or session.
#[derive(Debug, Clone)]
pub(crate) struct MplaWorkspaceBinding {
    pub(crate) run_id: RunId,
    pub(crate) payload_root: PathBuf,
    pub(crate) control_root: PathBuf,
    pub(crate) storage_admin_profile:
        sandbox_runtime_mpla_poc::storage_admin::StorageAdminCapabilityProfile,
    pub(crate) allocation: AllocationHandle,
    pub(crate) lease: MutableLease,
    pub(crate) lease_operation_id: OperationId,
    pub(crate) prepared: PreparedExternalSession,
    pub(crate) selected_ref: Option<PairedRefValue>,
    pub(crate) lower_dirs_newest_first: Vec<PathBuf>,
    pub(crate) mount_scope: Option<sandbox_runtime_mpla_poc::StorageAdminScope>,
    pub(crate) mount_receipt_binding:
        Option<sandbox_runtime_mpla_poc::storage_admin::StorageAdminMountReceiptBinding>,
    pub(crate) cleanup_operation_id: Option<OperationId>,
    pub(crate) phase: MplaStoragePhase,
}

/// The converged result for a holder-death cleanup transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderExitDisposition {
    Destroyed,
    RecoveryRequired { artifact: PathBuf },
    RetryableCleanupFailure { diagnostic: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderExitOutcome {
    pub workspace_session_id: WorkspaceSessionId,
    pub reason: String,
    pub disposition: HolderExitDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderLifecycleEventKind {
    ExitObserved,
    CleanupAttempt,
    CleanupFailure,
    CleanupTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderLifecycleEvent {
    pub sequence: u64,
    pub workspace_session_id: WorkspaceSessionId,
    pub kind: HolderLifecycleEventKind,
    pub detail: String,
    pub cleanup_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HolderLifecycleSnapshot {
    pub holder_exit_total: u64,
    pub cleanup_attempt_total: u64,
    pub cleanup_failure_total: u64,
    pub cleanup_terminal_total: u64,
    pub dropped_event_total: u64,
    pub events: Vec<HolderLifecycleEvent>,
}

impl FinalizationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Finalizing => "finalizing",
            Self::FinalizeFailed => "finalize_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceSession {
    pub workspace_session_id: WorkspaceSessionId,
    pub handle: WorkspaceHandle,
    pub cgroup_path: Option<PathBuf>,
    pub finalize_policy: FinalizePolicy,
    pub execution_scratch_route: ExecutionScratchRoute,
    pub active_commands: BTreeSet<NamespaceExecutionId>,
    pub finalization_state: FinalizationState,
    /// The sole holder supervisor observed this exact generation alive,
    /// claimed planned finalization, and reaped it before capture/publish.
    /// While the normal transaction is still running, the resulting expected
    /// exit must not be mistaken for an unexpected-exit recovery trigger.
    pub holder_quiesced_for_finalization: bool,
    pub holder_exit_recorded: bool,
    pub holder_cleanup_terminal: bool,
    pub holder_cleanup_attempts: u8,
    /// Per-resource destroy ledger. A later retry never invokes the raw
    /// workspace teardown again after it has succeeded just because cgroup
    /// cleanup remains pending.
    pub workspace_destroy_result: Option<DestroyWorkspaceResult>,
    pub cgroup_cleanup_complete: bool,
    /// Present only for sessions created through the dedicated MPLA public
    /// operation. Generic workspaces are never retrofitted into this mode.
    pub(crate) mpla_binding: Option<MplaWorkspaceBinding>,
}

impl WorkspaceSession {
    pub(crate) fn from_handle(
        handle: WorkspaceHandle,
        cgroup_path: Option<PathBuf>,
        finalize_policy: FinalizePolicy,
        execution_scratch_route: ExecutionScratchRoute,
    ) -> Self {
        let cgroup_cleanup_complete = cgroup_path.is_none();
        Self {
            workspace_session_id: handle.id.clone(),
            handle,
            cgroup_path,
            finalize_policy,
            execution_scratch_route,
            active_commands: BTreeSet::new(),
            finalization_state: FinalizationState::Active,
            holder_quiesced_for_finalization: false,
            holder_exit_recorded: false,
            holder_cleanup_terminal: false,
            holder_cleanup_attempts: 0,
            workspace_destroy_result: None,
            cgroup_cleanup_complete,
            mpla_binding: None,
        }
    }

    pub(crate) fn from_mpla_handle(
        handle: WorkspaceHandle,
        cgroup_path: Option<PathBuf>,
        binding: MplaWorkspaceBinding,
    ) -> Self {
        let mut session = Self::from_handle(
            handle,
            cgroup_path,
            FinalizePolicy::NoOp,
            ExecutionScratchRoute::WorkspaceScoped,
        );
        session.mpla_binding = Some(binding);
        session
    }

    pub(crate) fn handler(&self) -> WorkspaceSessionHandler {
        WorkspaceSessionHandler {
            workspace_session_id: self.workspace_session_id.clone(),
            handle: self.handle.clone(),
            cgroup_path: self.cgroup_path.clone(),
            execution_scratch_route: self.execution_scratch_route,
        }
    }
}

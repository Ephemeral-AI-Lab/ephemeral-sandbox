use std::collections::BTreeSet;
use std::path::PathBuf;

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

/// Lifecycle phase of a session's finalization. `FinalizeFailed` and a session
/// stuck in `Finalizing` are destroyable through `guarded_destroy` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizationState {
    Active,
    Finalizing,
    FinalizeFailed,
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
        }
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

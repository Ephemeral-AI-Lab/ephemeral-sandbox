//! Workspace manager.
//!
//! The manager owns admission policy, persistence, and the lifecycle
//! modules own network-mode-specific setup, shared holder, overlay, teardown,
//! and persistence behavior.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sandbox_observability_telemetry::Observer;
use serde::Deserialize;

use crate::isolated_network_setup::IsolatedNetwork;
use crate::lifecycle::destroy::TeardownTransaction;
use crate::model::{WorkspaceHandle, WorkspaceOwnershipSnapshot, WorkspaceSessionId};
use crate::namespace::holder::HolderRegistration;
use crate::namespace::NamespaceRuntime;
pub use crate::session::{HolderNsFds, MountedWorkspace};

pub use crate::lifecycle::ExitOutcome;

pub(crate) const PERSISTED_HANDLES_SCHEMA_VERSION: u32 = 2;
const COMPLETED_TEARDOWN_CAPACITY: usize = 128;

#[derive(Clone)]
pub(crate) struct CompletedTeardown {
    workspace_session_id: WorkspaceSessionId,
    holder_registration: HolderRegistration,
    outcome: ExitOutcome,
}

impl CompletedTeardown {
    fn matches(&self, handle: &WorkspaceHandle) -> bool {
        handle.matches_holder_generation(&self.workspace_session_id, &self.holder_registration)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceCaps {
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
    /// Freeze-poll budget for the remount quiesce, in seconds.
    pub freeze_budget_s: f64,
}

impl Default for ResourceCaps {
    fn default() -> Self {
        Self {
            setup_timeout_s: 30.0,
            exit_grace_s: 0.25,
            rfc1918_egress: Rfc1918Egress::Allow,
            freeze_budget_s: 0.5,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceManagerError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("workspace session is not open")]
    NotOpen,

    #[error("workspace session is already open: {workspace_session_id:?}")]
    AlreadyOpen {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("setup failed at step {step}")]
    SetupFailed { step: String },

    #[error("isolated network unavailable: {0}")]
    NetworkUnavailable(String),

    #[error("workspace teardown remains retryable for {workspace_session_id:?}: {failures:?}")]
    TeardownFailed {
        workspace_session_id: WorkspaceSessionId,
        failures: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceShutdownFailure {
    pub workspace_session_id: WorkspaceSessionId,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceManagerShutdownReport {
    pub attempted_workspace_ids: Vec<WorkspaceSessionId>,
    pub closed_workspace_ids: Vec<WorkspaceSessionId>,
    pub retryable_failures: Vec<WorkspaceShutdownFailure>,
    pub remaining_workspace_ids: Vec<WorkspaceSessionId>,
}

impl WorkspaceManagerShutdownReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.retryable_failures.is_empty() && self.remaining_workspace_ids.is_empty()
    }
}

pub struct WorkspaceManager {
    pub(crate) workspace_root: String,
    pub(crate) caps: ResourceCaps,
    pub(crate) runtime: Arc<NamespaceRuntime>,
    pub(crate) network: IsolatedNetwork,
    pub(crate) scratch_root: PathBuf,
    pub(crate) scratch_locator: crate::scratch::WorkspaceScratchLocator,
    /// Bound by [`crate::WorkspaceRuntimeService`] before the manager can
    /// create or destroy a workspace. Keeping the root on the teardown owner
    /// lets lease release participate in the same retryable transaction as
    /// holder, fd, mount, scratch, and persisted-handle cleanup.
    pub(crate) layer_stack_root: Option<PathBuf>,
    pub(crate) handles: HashMap<WorkspaceSessionId, MountedWorkspace>,
    pub(crate) teardowns: HashMap<WorkspaceSessionId, TeardownTransaction>,
    pub(crate) completed_teardowns: VecDeque<CompletedTeardown>,
}

impl WorkspaceManager {
    #[must_use]
    pub fn new(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_root: PathBuf,
        obs: Observer,
    ) -> Self {
        let scratch_locator = crate::scratch::WorkspaceScratchLocator::new(scratch_root.clone())
            .expect("workspace scratch root must be valid");
        let runtime = NamespaceRuntime::new(caps.setup_timeout_s, obs);
        Self::with_runtime_and_locator(workspace_root, caps, scratch_locator, runtime)
    }

    #[must_use]
    pub fn with_scratch_locator(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_locator: crate::scratch::WorkspaceScratchLocator,
        obs: Observer,
    ) -> Self {
        let runtime = NamespaceRuntime::new(caps.setup_timeout_s, obs);
        Self::with_runtime_and_locator(workspace_root, caps, scratch_locator, runtime)
    }

    fn with_runtime_and_locator(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_locator: crate::scratch::WorkspaceScratchLocator,
        runtime: NamespaceRuntime,
    ) -> Self {
        let network = IsolatedNetwork::new(caps.rfc1918_egress);
        let scratch_root = scratch_locator.root().to_path_buf();
        Self {
            workspace_root: workspace_root.into(),
            caps,
            runtime: Arc::new(runtime),
            network,
            scratch_root,
            scratch_locator,
            layer_stack_root: None,
            handles: HashMap::with_capacity(1),
            teardowns: HashMap::with_capacity(1),
            completed_teardowns: VecDeque::with_capacity(COMPLETED_TEARDOWN_CAPACITY),
        }
    }

    pub(crate) fn bind_layer_stack_root(&mut self, layer_stack_root: PathBuf) {
        self.layer_stack_root = Some(layer_stack_root);
    }

    pub(crate) fn take_holder_exit_subscription(
        &self,
    ) -> Result<crate::service::HolderExitSubscription, String> {
        self.runtime.take_holder_exit_subscription()
    }

    pub(crate) fn handle(&self, workspace_id: &WorkspaceSessionId) -> Option<&MountedWorkspace> {
        self.handles.get(workspace_id)
    }

    pub(crate) fn replace_candidate_lease(
        &mut self,
        workspace_id: &WorkspaceSessionId,
        lease: sandbox_runtime_layerstack::service::CandidateGenerationLease,
    ) -> Result<(), WorkspaceManagerError> {
        let handle = self
            .handles
            .get_mut(workspace_id)
            .ok_or(WorkspaceManagerError::NotOpen)?;
        let admission = handle.candidate_admission.as_mut().ok_or_else(|| {
            WorkspaceManagerError::InvalidArgument(
                "workspace has no candidate generation lease".to_owned(),
            )
        })?;
        if admission.lease.lease_id != lease.lease_id
            || admission.lease.materialization_id != lease.materialization_id
            || admission.lease.generation != lease.generation
            || admission.lease.fence != lease.fence
        {
            return Err(WorkspaceManagerError::InvalidArgument(
                "renewed candidate lease changed exact generation identity".to_owned(),
            ));
        }
        admission.lease = lease;
        self.persist_handles()
    }

    pub(crate) fn owns_handle_generation(&self, handle: &WorkspaceHandle) -> bool {
        self.handles
            .get(&handle.id)
            .or_else(|| {
                self.teardowns
                    .get(&handle.id)
                    .map(TeardownTransaction::owned_handle)
            })
            .is_some_and(|mounted| handle.matches_mounted_workspace(mounted))
            || self
                .completed_teardowns
                .iter()
                .any(|completed| completed.matches(handle))
    }

    pub(crate) fn completed_teardown_outcome(
        &self,
        handle: &WorkspaceHandle,
    ) -> Option<ExitOutcome> {
        self.completed_teardowns
            .iter()
            .find(|completed| completed.matches(handle))
            .map(|completed| completed.outcome.clone())
    }

    pub(crate) fn record_completed_teardown(
        &mut self,
        workspace_session_id: WorkspaceSessionId,
        holder_registration: HolderRegistration,
        outcome: ExitOutcome,
    ) {
        self.completed_teardowns.retain(|completed| {
            completed.workspace_session_id != workspace_session_id
                || completed.holder_registration != holder_registration
        });
        if self.completed_teardowns.len() == COMPLETED_TEARDOWN_CAPACITY {
            self.completed_teardowns.pop_front();
        }
        self.completed_teardowns.push_back(CompletedTeardown {
            workspace_session_id,
            holder_registration,
            outcome,
        });
    }

    pub(crate) fn forget_completed_teardowns(&mut self, workspace_session_id: &WorkspaceSessionId) {
        self.completed_teardowns
            .retain(|completed| completed.workspace_session_id != *workspace_session_id);
    }

    pub(crate) fn forget_completed_teardown(&mut self, handle: &WorkspaceHandle) {
        self.completed_teardowns
            .retain(|completed| !completed.matches(handle));
    }

    pub(crate) fn ensure_workspace_available(
        &self,
        workspace_id: &WorkspaceSessionId,
    ) -> Result<(), WorkspaceManagerError> {
        if self.handles.contains_key(workspace_id) || self.teardowns.contains_key(workspace_id) {
            return Err(WorkspaceManagerError::AlreadyOpen {
                workspace_session_id: workspace_id.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn workspace_session_root(&self, workspace_id: &WorkspaceSessionId) -> PathBuf {
        self.scratch_locator
            .session_root(workspace_id)
            .expect("workspace ids are validated before scratch lookup")
    }

    pub(crate) fn owned_handles(&self) -> impl Iterator<Item = &MountedWorkspace> {
        self.handles.values().chain(
            self.teardowns
                .values()
                .map(TeardownTransaction::owned_handle),
        )
    }

    pub(crate) fn pending_teardown_ids(&self) -> Vec<WorkspaceSessionId> {
        let mut ids = self.teardowns.keys().cloned().collect::<Vec<_>>();
        ids.sort_by(|left, right| left.0.cmp(&right.0));
        ids
    }

    pub(crate) fn ownership_snapshot(&self) -> WorkspaceOwnershipSnapshot {
        let mut snapshot = WorkspaceOwnershipSnapshot::default();
        for handle in self.owned_handles() {
            snapshot.namespace_fd_count = snapshot
                .namespace_fd_count
                .saturating_add(handle.ns_fds.len());
            snapshot.control_fd_count = snapshot
                .control_fd_count
                .saturating_add(usize::from(handle.readiness_fd >= 0))
                .saturating_add(usize::from(handle.control_fd >= 0));
            snapshot.active_scratch_directories = snapshot
                .active_scratch_directories
                .saturating_add(usize::from(handle.dirs.run_dir.is_dir()));
        }
        snapshot.persisted_workspace_handles = self.handles.len().saturating_add(
            self.teardowns
                .values()
                .filter(|transaction| transaction.has_persisted_handle())
                .count(),
        );
        snapshot
    }

    /// The isolated-network IP of a mounted workspace, when it has one. Shared
    /// workspaces and workspaces without a veth allocation yield `None`.
    #[must_use]
    pub fn isolated_ip(&self, workspace_id: &WorkspaceSessionId) -> Option<std::net::Ipv4Addr> {
        self.handles
            .get(workspace_id)
            .and_then(|workspace| workspace.veth.as_ref())
            .map(|veth| veth.ns_ip)
    }
}

impl Drop for WorkspaceManager {
    fn drop(&mut self) {
        let _ = self.shutdown_all();
        let _ = self.runtime.shutdown();
    }
}

pub(crate) fn validate_workspace_root(workspace_root: &str) -> Result<(), WorkspaceManagerError> {
    let workspace_root = workspace_root.trim();
    if workspace_root.is_empty() {
        return Err(WorkspaceManagerError::InvalidArgument(
            "workspace_root is required".to_owned(),
        ));
    }
    if !Path::new(workspace_root).is_absolute() {
        return Err(WorkspaceManagerError::InvalidArgument(format!(
            "workspace_root must be absolute: {workspace_root}"
        )));
    }
    Ok(())
}

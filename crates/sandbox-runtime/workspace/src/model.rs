use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use sandbox_runtime_namespace_execution::NamespaceTarget;
use sandbox_runtime_namespace_process::runner::protocol::{Fd, NsFds};

use crate::overlay::tree::TreeResourceStats;
use crate::session::{HolderNsFds, MountedWorkspace};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceSessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaseId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRevision {
    pub version: i64,
    pub root_hash: String,
    pub layer_count: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LayerStackSnapshotView {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_paths: Vec<PathBuf>,
}

impl fmt::Debug for LayerStackSnapshotView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LayerStackSnapshotView")
            .field("manifest_version", &self.manifest_version)
            .field("root_hash", &self.root_hash)
            .field("layer_count", &self.layer_paths.len())
            .finish()
    }
}

impl From<sandbox_runtime_layerstack::service::Snapshot> for LayerStackSnapshotView {
    fn from(snapshot: sandbox_runtime_layerstack::service::Snapshot) -> Self {
        Self {
            manifest_version: snapshot.manifest_version,
            root_hash: snapshot.root_hash,
            layer_paths: snapshot.layer_paths,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LayerStackSnapshotRef {
    pub lease_id: LeaseId,
    pub manifest_version: i64,
    pub root_hash: String,
    pub manifest: sandbox_runtime_layerstack::Manifest,
    pub layer_paths: Vec<PathBuf>,
}

impl fmt::Debug for LayerStackSnapshotRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LayerStackSnapshotRef")
            .field("lease_id", &self.lease_id)
            .field("manifest_version", &self.manifest_version)
            .field("root_hash", &self.root_hash)
            .field("manifest_layer_count", &self.manifest.layers.len())
            .field("layer_count", &self.layer_paths.len())
            .finish()
    }
}

impl LayerStackSnapshotRef {
    #[must_use]
    pub fn base_revision(&self) -> BaseRevision {
        BaseRevision {
            version: self.manifest_version,
            root_hash: self.root_hash.clone(),
            layer_count: self.layer_paths.len(),
        }
    }
}

impl From<sandbox_runtime_layerstack::Lease> for LayerStackSnapshotRef {
    fn from(lease: sandbox_runtime_layerstack::Lease) -> Self {
        let manifest_version = lease.manifest_version();
        let root_hash = lease.root_hash();
        Self {
            lease_id: LeaseId(lease.lease_id),
            manifest_version,
            root_hash,
            manifest: lease.manifest,
            layer_paths: lease.layer_paths,
        }
    }
}

/// Network boundary applied to a private mounted workspace.
///
/// This selector encodes one axis only: whether the workspace shares the host
/// network namespace or gets a dedicated, isolated one. Every workspace is
/// otherwise isolated (private overlay plus mount/pid/user namespaces)
/// regardless of this value. It does not encode lifecycle length, publication
/// behavior, or a finalize policy; those decisions belong to the runtime or
/// operation layer that owns the handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProfile {
    /// Shared network: the workspace joins the host network namespace (host
    /// loopback and interfaces are visible). Mount, pid, and user namespaces
    /// stay isolated — this is not the host.
    Shared,
    /// Isolated network: the workspace gets a dedicated network namespace with
    /// veth and bridge-port isolation.
    Isolated,
}

impl NetworkProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Isolated => "isolated",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceHandle {
    pub id: WorkspaceSessionId,
    pub workspace_root: PathBuf,
    pub network: NetworkProfile,
    pub snapshot: LayerStackSnapshotRef,
    pub holder_pid: i32,
    launch: Option<WorkspaceLaunchContext>,
}

impl fmt::Debug for WorkspaceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceHandle")
            .field("id", &self.id)
            .field("workspace_root", &self.workspace_root)
            .field("network", &self.network)
            .field("snapshot", &self.snapshot)
            .field("launch", &self.launch.as_ref().map(|_| "<available>"))
            .finish()
    }
}

impl WorkspaceHandle {
    pub fn entry(&self) -> Result<WorkspaceEntry, WorkspaceEntryError> {
        self.launch
            .as_ref()
            .ok_or_else(WorkspaceEntryError::missing_launch_material)?
            .entry()
    }

    #[must_use]
    pub fn base_revision(&self) -> BaseRevision {
        self.snapshot.base_revision()
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn holder_backed_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        network: NetworkProfile,
        snapshot: LayerStackSnapshotRef,
        upperdir: PathBuf,
        workdir: PathBuf,
    ) -> Self {
        Self::with_launch_for_test(
            id,
            workspace_root.clone(),
            network,
            snapshot.clone(),
            Some(launch_context_for_test(
                network,
                workspace_root,
                snapshot.layer_paths.clone(),
                upperdir,
                workdir,
                Some(HolderNsFds {
                    user: Some(10),
                    mnt: Some(11),
                    pid: Some(12),
                    net: (network == NetworkProfile::Isolated).then_some(13),
                }),
            )),
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn unavailable_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        network: NetworkProfile,
        snapshot: LayerStackSnapshotRef,
        upperdir: PathBuf,
        workdir: PathBuf,
    ) -> Self {
        Self::with_launch_for_test(
            id,
            workspace_root.clone(),
            network,
            snapshot.clone(),
            Some(launch_context_for_test(
                network,
                workspace_root,
                snapshot.layer_paths.clone(),
                upperdir,
                workdir,
                None,
            )),
        )
    }

    #[must_use]
    pub fn without_launch_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        network: NetworkProfile,
        snapshot: LayerStackSnapshotRef,
    ) -> Self {
        Self::with_launch_for_test(id, workspace_root, network, snapshot, None)
    }

    fn with_launch_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        network: NetworkProfile,
        snapshot: LayerStackSnapshotRef,
        launch: Option<WorkspaceLaunchContext>,
    ) -> Self {
        let holder_pid = launch
            .as_ref()
            .and_then(|context| context.holder_fds)
            .map_or(0, |_| i32::try_from(std::process::id()).unwrap_or(i32::MAX));
        Self {
            id,
            workspace_root,
            network,
            snapshot,
            holder_pid,
            launch,
        }
    }
}

fn launch_context_for_test(
    network: NetworkProfile,
    workspace_root: PathBuf,
    layer_paths: Vec<PathBuf>,
    upperdir: PathBuf,
    workdir: PathBuf,
    holder_fds: Option<HolderNsFds>,
) -> WorkspaceLaunchContext {
    WorkspaceLaunchContext {
        network,
        workspace_root,
        layer_paths,
        upperdir,
        workdir,
        holder_fds,
    }
}

#[derive(Clone, PartialEq, Eq)]
struct WorkspaceLaunchContext {
    network: NetworkProfile,
    workspace_root: PathBuf,
    layer_paths: Vec<PathBuf>,
    upperdir: PathBuf,
    workdir: PathBuf,
    holder_fds: Option<HolderNsFds>,
}

impl WorkspaceLaunchContext {
    fn entry(&self) -> Result<WorkspaceEntry, WorkspaceEntryError> {
        Ok(WorkspaceEntry {
            workspace_root: self.workspace_root.clone(),
            layer_paths: self.layer_paths.clone(),
            upperdir: self.upperdir.clone(),
            workdir: self.workdir.clone(),
            ns_fds: self.required_holder_fds()?,
        })
    }

    fn required_holder_fds(&self) -> Result<WorkspaceEntryFds, WorkspaceEntryError> {
        let Some(fds) = self.holder_fds else {
            return Err(WorkspaceEntryError::incomplete());
        };
        let (Some(user), Some(mnt), Some(pid)) = (fds.user, fds.mnt, fds.pid) else {
            return Err(WorkspaceEntryError::incomplete());
        };
        if self.network == NetworkProfile::Isolated && fds.net.is_none() {
            return Err(WorkspaceEntryError::incomplete());
        }
        Ok(WorkspaceEntryFds {
            user,
            mnt,
            pid,
            net: fds.net,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub workspace_root: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
    pub ns_fds: WorkspaceEntryFds,
}

impl fmt::Debug for WorkspaceEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceEntry")
            .field("storage", &"<hidden>")
            .field("holder_context", &self.ns_fds)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceEntryFds {
    pub user: i32,
    pub mnt: i32,
    pub pid: i32,
    pub net: Option<i32>,
}

impl fmt::Debug for WorkspaceEntryFds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let available = |fd: Option<i32>| fd.map(|_| "<available>");
        f.debug_struct("WorkspaceEntryFds")
            .field("user", &"<available>")
            .field("mnt", &"<available>")
            .field("pid", &"<available>")
            .field("net", &available(self.net))
            .finish()
    }
}

impl From<WorkspaceEntryFds> for NsFds {
    fn from(fds: WorkspaceEntryFds) -> Self {
        Self {
            user: Some(Fd(fds.user)),
            mnt: Some(Fd(fds.mnt)),
            pid: Some(Fd(fds.pid)),
            net: fds.net.map(Fd),
        }
    }
}

impl From<WorkspaceEntry> for NamespaceTarget {
    fn from(entry: WorkspaceEntry) -> Self {
        Self {
            workspace_root: entry.workspace_root,
            layer_paths: entry.layer_paths,
            upperdir: Some(entry.upperdir),
            workdir: Some(entry.workdir),
            ns_fds: entry.ns_fds.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntryError {
    message: String,
}

impl WorkspaceEntryError {
    fn missing_launch_material() -> Self {
        Self {
            message: "resolved workspace lacks workspace entry material".to_owned(),
        }
    }

    fn incomplete() -> Self {
        Self {
            message: "workspace entry context is incomplete".to_owned(),
        }
    }
}

impl fmt::Display for WorkspaceEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkspaceEntryError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkspaceRequest {
    pub network: NetworkProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChangesRequest {
    pub include_stats: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedPathKind {
    Write,
    Delete,
    Symlink,
    OpaqueDir,
}

impl From<&sandbox_runtime_layerstack::LayerChange> for ChangedPathKind {
    fn from(change: &sandbox_runtime_layerstack::LayerChange) -> Self {
        match change {
            sandbox_runtime_layerstack::LayerChange::Write { .. }
            | sandbox_runtime_layerstack::LayerChange::WriteFile { .. } => Self::Write,
            sandbox_runtime_layerstack::LayerChange::Delete { .. } => Self::Delete,
            sandbox_runtime_layerstack::LayerChange::Symlink { .. } => Self::Symlink,
            sandbox_runtime_layerstack::LayerChange::OpaqueDir { .. } => Self::OpaqueDir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedPathDropReason {
    UnsupportedSpecialFile,
    InvalidLayerPath,
    CommandScratchPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedPathDrop {
    pub path: String,
    pub reason: ProtectedPathDropReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedWorkspaceChanges {
    pub workspace_session_id: WorkspaceSessionId,
    pub base_revision: BaseRevision,
    pub base_manifest: sandbox_runtime_layerstack::Manifest,
    pub changed_paths: Vec<String>,
    pub changed_path_kinds: BTreeMap<String, ChangedPathKind>,
    pub protected_drops: Vec<ProtectedPathDrop>,
    pub stats: Option<TreeResourceStats>,
    pub changes: Vec<sandbox_runtime_layerstack::LayerChange>,
    pub metadata_path_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlySnapshotHandle {
    pub view_root: PathBuf,
    pub generation_key: String,
    pub snapshot: LayerStackSnapshotView,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DestroyWorkspaceRequest {
    pub grace_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DestroyWorkspaceResult {
    pub workspace_session_id: WorkspaceSessionId,
    pub evicted_upperdir_bytes: u64,
    pub lifetime_s: f64,
    pub lease_released: Option<bool>,
    pub lease_release_error: Option<String>,
    pub active_leases_after: usize,
}

impl From<&MountedWorkspace> for WorkspaceHandle {
    fn from(handle: &MountedWorkspace) -> Self {
        Self {
            id: handle.workspace_id.clone(),
            workspace_root: PathBuf::from(&handle.workspace_root),
            network: handle.network,
            snapshot: handle.snapshot.clone(),
            holder_pid: handle.holder_pid,
            launch: Some(WorkspaceLaunchContext {
                network: handle.network,
                workspace_root: PathBuf::from(&handle.workspace_root),
                layer_paths: handle.snapshot.layer_paths.clone(),
                upperdir: handle.dirs.upperdir.clone(),
                workdir: handle.dirs.workdir.clone(),
                holder_fds: (!handle.ns_fds.is_empty()).then_some(handle.ns_fds),
            }),
        }
    }
}

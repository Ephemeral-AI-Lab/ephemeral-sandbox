use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use sandbox_runtime_namespace_execution::NamespaceTarget;
use sandbox_runtime_namespace_process::runner::protocol::{Fd, NsFds};

use crate::overlay::tree::TreeResourceStats;
use crate::profile::{WorkspaceModeFds, WorkspaceModeHandle};

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

impl From<sandbox_runtime_layerstack::service::LeasedSnapshot> for LayerStackSnapshotRef {
    fn from(snapshot: sandbox_runtime_layerstack::service::LeasedSnapshot) -> Self {
        Self {
            lease_id: LeaseId(snapshot.lease_id),
            manifest_version: snapshot.manifest_version,
            root_hash: snapshot.root_hash,
            manifest: snapshot.manifest,
            layer_paths: snapshot.layer_paths,
        }
    }
}

/// Workspace environment profile for a private mounted workspace.
///
/// The selector reflects the current concrete split: whether the workspace
/// uses the host-compatible network path or adds a dedicated network boundary.
/// It does not encode lifecycle length, publication behavior, or whether the
/// caller is running a one-shot operation. Those decisions belong to the
/// runtime or operation layer that owns the workspace handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceProfile {
    /// Host-compatible profile: private overlay and holder namespace stack
    /// without a dedicated network boundary.
    HostCompatible,
    /// Fully isolated profile: private overlay and holder namespace stack plus
    /// a dedicated network boundary.
    Isolated,
}

impl WorkspaceProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HostCompatible => "host_compatible",
            Self::Isolated => "isolated",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceHandle {
    pub id: WorkspaceSessionId,
    pub workspace_root: PathBuf,
    pub profile: WorkspaceProfile,
    pub base_revision: BaseRevision,
    pub snapshot: LayerStackSnapshotRef,
    launch: Option<WorkspaceLaunchContext>,
}

impl fmt::Debug for WorkspaceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceHandle")
            .field("id", &self.id)
            .field("workspace_root", &self.workspace_root)
            .field("profile", &self.profile)
            .field("base_revision", &self.base_revision)
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
    #[allow(clippy::too_many_arguments)]
    pub fn holder_backed_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        profile: WorkspaceProfile,
        snapshot: LayerStackSnapshotRef,
        upperdir: PathBuf,
        workdir: PathBuf,
    ) -> Self {
        Self::with_launch_for_test(
            id,
            workspace_root.clone(),
            profile,
            snapshot.clone(),
            Some(launch_context_for_test(
                profile,
                workspace_root,
                snapshot.layer_paths.clone(),
                upperdir,
                workdir,
                Some(WorkspaceLaunchFds {
                    user: Some(10),
                    mnt: Some(11),
                    pid: Some(12),
                    net: (profile == WorkspaceProfile::Isolated).then_some(13),
                }),
            )),
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn unavailable_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        profile: WorkspaceProfile,
        snapshot: LayerStackSnapshotRef,
        upperdir: PathBuf,
        workdir: PathBuf,
    ) -> Self {
        Self::with_launch_for_test(
            id,
            workspace_root.clone(),
            profile,
            snapshot.clone(),
            Some(launch_context_for_test(
                profile,
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
        profile: WorkspaceProfile,
        snapshot: LayerStackSnapshotRef,
    ) -> Self {
        Self::with_launch_for_test(id, workspace_root, profile, snapshot, None)
    }

    fn with_launch_for_test(
        id: WorkspaceSessionId,
        workspace_root: PathBuf,
        profile: WorkspaceProfile,
        snapshot: LayerStackSnapshotRef,
        launch: Option<WorkspaceLaunchContext>,
    ) -> Self {
        Self {
            id,
            workspace_root,
            profile,
            base_revision: snapshot.base_revision(),
            snapshot,
            launch,
        }
    }
}

fn launch_context_for_test(
    profile: WorkspaceProfile,
    workspace_root: PathBuf,
    layer_paths: Vec<PathBuf>,
    upperdir: PathBuf,
    workdir: PathBuf,
    holder_fds: Option<WorkspaceLaunchFds>,
) -> WorkspaceLaunchContext {
    WorkspaceLaunchContext {
        profile,
        workspace_root,
        layer_paths,
        upperdir,
        workdir,
        holder_fds,
    }
}

#[derive(Clone, PartialEq, Eq)]
struct WorkspaceLaunchContext {
    profile: WorkspaceProfile,
    workspace_root: PathBuf,
    layer_paths: Vec<PathBuf>,
    upperdir: PathBuf,
    workdir: PathBuf,
    holder_fds: Option<WorkspaceLaunchFds>,
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
        if self.profile == WorkspaceProfile::Isolated && fds.net.is_none() {
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct WorkspaceLaunchFds {
    user: Option<i32>,
    mnt: Option<i32>,
    pid: Option<i32>,
    net: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkspaceRequest {
    pub profile: WorkspaceProfile,
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
pub struct RemountWorkspaceRequest {
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemountWorkspaceResult {
    pub handle: WorkspaceHandle,
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

impl From<&WorkspaceModeHandle> for WorkspaceHandle {
    fn from(handle: &WorkspaceModeHandle) -> Self {
        Self {
            id: WorkspaceSessionId(handle.workspace_id.0.clone()),
            workspace_root: PathBuf::from(&handle.workspace_root),
            profile: handle.profile,
            base_revision: BaseRevision {
                version: handle.manifest_version,
                root_hash: handle.manifest_root_hash.clone(),
                layer_count: handle.layer_paths.len(),
            },
            snapshot: LayerStackSnapshotRef {
                lease_id: LeaseId(handle.lease_id.clone()),
                manifest_version: handle.manifest_version,
                root_hash: handle.manifest_root_hash.clone(),
                manifest: handle.base_manifest.clone(),
                layer_paths: handle.layer_paths.clone(),
            },
            launch: Some(WorkspaceLaunchContext {
                profile: handle.profile,
                workspace_root: PathBuf::from(&handle.workspace_root),
                layer_paths: handle.layer_paths.clone(),
                upperdir: handle.dirs.upperdir.clone(),
                workdir: handle.dirs.workdir.clone(),
                holder_fds: holder_fds_from_mode(handle.ns_fds),
            }),
        }
    }
}

fn holder_fds_from_mode(ns_fds: WorkspaceModeFds) -> Option<WorkspaceLaunchFds> {
    if ns_fds.is_empty() {
        return None;
    }
    Some(WorkspaceLaunchFds {
        user: ns_fds.user,
        mnt: ns_fds.mnt,
        pid: ns_fds.pid,
        net: ns_fds.net,
    })
}

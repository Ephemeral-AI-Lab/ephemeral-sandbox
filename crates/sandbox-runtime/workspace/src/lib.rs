//! Shared workspace runtime primitives plus concrete workspace isolation
//! profiles.
//!
//! Every profile creates a private mounted workspace: fresh overlay directories
//! plus the holder-owned namespace stack used to run and remount commands.
//! `WorkspaceProfile` selects the isolation profile applied to that workspace; higher
//! layers decide when a workspace is created, destroyed, captured, or published.
//!
//! The host-compatible profile keeps the private workspace overlay and holder
//! namespace stack without adding a dedicated network boundary. The isolated
//! profile adds a dedicated network boundary with veth and network policy.
//! `overlay` holds the filesystem contracts both profiles share, while common
//! lifecycle code owns holder, namespace FD, scratch, and teardown behavior.
#![forbid(unsafe_code)]

pub mod error;
mod isolated_setup;
mod lifecycle;
pub mod model;
mod namespace;
pub mod overlay;
pub mod profile;
pub mod service;

pub use error::WorkspaceError;
pub use model::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef,
    LayerStackSnapshotView, LeaseId, ProtectedPathDrop, ProtectedPathDropReason,
    ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult, WorkspaceEntry,
    WorkspaceEntryError, WorkspaceEntryFds, WorkspaceHandle, WorkspaceProfile, WorkspaceSessionId,
};
pub use service::{WorkspaceRuntimeHooks, WorkspaceRuntimeService};

/// Internal namespace types surfaced to this crate's `tests/` suites; available
/// only under the `test-support` feature.
#[cfg(feature = "test-support")]
pub mod test_support {
    use std::path::PathBuf;
    use std::sync::Arc;

    use sandbox_runtime_namespace_execution::NamespaceExecutionEngine;

    use crate::lifecycle::remount::RemountProbe;
    use crate::profile::{
        ResourceCaps, WorkspaceModeError, WorkspaceModeHandle, WorkspaceModeId,
        WorkspaceModeManager,
    };

    pub use crate::lifecycle::remount::WorkspaceRemountState;
    pub use crate::namespace::NamespaceRuntime;

    pub fn namespace_runtime_with_engine_for_test(
        engine: Arc<NamespaceExecutionEngine>,
    ) -> NamespaceRuntime {
        NamespaceRuntime::from_engine_for_test(engine)
    }

    pub fn mount_overlay_for_test(
        runtime: &NamespaceRuntime,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
    ) -> Result<(), WorkspaceModeError> {
        runtime.mount_overlay_via_engine(handle, layer_paths)
    }

    pub fn remount_overlay_for_test(
        runtime: &NamespaceRuntime,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
        probe: &RemountProbe,
    ) -> Result<crate::profile::RemountOverlayResult, WorkspaceModeError> {
        runtime.remount_overlay_via_engine(handle, layer_paths, probe)
    }

    pub fn workspace_mode_manager_with_runtime_for_test(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_root: PathBuf,
        runtime: NamespaceRuntime,
    ) -> WorkspaceModeManager {
        WorkspaceModeManager::with_runtime(workspace_root, caps, scratch_root, runtime)
    }

    pub fn insert_handle_for_test(manager: &mut WorkspaceModeManager, handle: WorkspaceModeHandle) {
        manager.handles.insert(handle.workspace_id.clone(), handle);
    }

    pub fn remount_with_layers_for_test(
        manager: &mut WorkspaceModeManager,
        workspace_id: &WorkspaceModeId,
        layer_paths: Vec<PathBuf>,
        probe: &RemountProbe,
    ) -> Result<WorkspaceModeHandle, WorkspaceModeError> {
        manager.remount_with_layers(workspace_id, layer_paths, probe)
    }
}

//! Shared workspace runtime primitives plus concrete workspace modes.
//!
//! `ephemeral_workspace` owns one-operation overlay transactions that publish
//! captured upperdir changes. `isolated_workspace` owns caller-keyed private
//! namespaces whose upperdir is discarded on exit. The `capture`, `dirs`, and
//! `tree` modules hold the filesystem and telemetry contracts both modes share
//! so they expose the same core operation vocabulary without hiding their
//! different lifecycle rules.
#![forbid(unsafe_code)]

pub mod capture;
pub mod dirs;
pub mod ephemeral_workspace;
pub mod error;
mod isolated_network_setup;
pub mod isolated_workspace;
mod lifecycle;
pub mod model;
mod namespace;
mod network_mode;
mod overlay;
pub mod service;
pub mod tree;

pub use capture::{
    capture_upperdir, capture_upperdir_for_snapshot_with_options, capture_upperdir_with_payloads,
    CaptureError, CapturedChanges, RoutedCapturedChanges,
};
pub use dirs::{DirAllocationError, OverlayDirs, OverlayDirsGuard};
pub use error::WorkspaceError;
// Compatibility export for existing isolated-workspace callers during the
// unified workspace migration. The new public scaffold handle is available as
// `workspace::model::WorkspaceHandle` and `workspace::UnifiedWorkspaceHandle`.
pub use isolated_workspace::WorkspaceHandle;
pub use model::{
    BaseRevision, CallerId, CaptureChangesRequest, CaptureChangesResult, ChangedPathKind,
    CommandStatus, CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult,
    NetworkMode, ProtectedPathDrop, ProtectedPathDropReason, RunCommandRequest, RunCommandResult,
    WorkspaceHandle as UnifiedWorkspaceHandle, WorkspaceId,
};
pub use network_mode::host::{overlay_run_dirs, EphemeralWorkspace, EphemeralWorkspaceError};
pub use network_mode::isolated_network::{
    DnsConfiguration, ExitOutcome, IsolatedError, IsolatedManager, IsolatedSnapshot,
    IsolatedWorkspaceBinding, IsolatedWorkspaceHandle, IsolatedWorkspaceId, RemountOverlayReport,
    RemountProbe, RemountedWorkspace, ResourceCaps, Rfc1918Egress, WorkspaceRemountState,
};
pub use service::WorkspaceService;
pub use tree::TreeResourceStats;

#[cfg(test)]
mod tests {
    #[test]
    fn root_handle_exports_preserve_migration_aliases() {
        fn legacy_alias(
            handle: crate::WorkspaceHandle,
        ) -> crate::isolated_workspace::WorkspaceHandle {
            handle
        }

        fn unified_alias(handle: crate::UnifiedWorkspaceHandle) -> crate::model::WorkspaceHandle {
            handle
        }

        let _: fn(crate::WorkspaceHandle) -> crate::isolated_workspace::WorkspaceHandle =
            legacy_alias;
        let _: fn(crate::UnifiedWorkspaceHandle) -> crate::model::WorkspaceHandle = unified_alias;
    }
}

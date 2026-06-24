use crate::error::WorkspaceError;
use crate::model::{
    CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, DestroyWorkspaceResult, ReadonlySnapshotHandle, WorkspaceHandle,
};

#[doc(hidden)]
pub struct WorkspaceRuntimeHooks {
    pub create_workspace: Box<
        dyn Fn(CreateWorkspaceRequest) -> Result<WorkspaceHandle, WorkspaceError> + Send + Sync,
    >,
    #[expect(
        clippy::type_complexity,
        reason = "hook signatures stay explicit by policy"
    )]
    pub capture_changes: Box<
        dyn Fn(
                &WorkspaceHandle,
                CaptureChangesRequest,
            ) -> Result<CapturedWorkspaceChanges, WorkspaceError>
            + Send
            + Sync,
    >,
    pub destroy_workspace: Box<
        dyn Fn(
                WorkspaceHandle,
                DestroyWorkspaceRequest,
            ) -> Result<DestroyWorkspaceResult, WorkspaceError>
            + Send
            + Sync,
    >,
    pub latest_snapshot:
        Box<dyn Fn() -> Result<ReadonlySnapshotHandle, WorkspaceError> + Send + Sync>,
}

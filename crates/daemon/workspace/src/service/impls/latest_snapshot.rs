use crate::error::WorkspaceError;
use crate::model::{LatestSnapshotRequest, ReadonlySnapshotHandle};
use crate::service::support::{ensure_absolute, ensure_non_empty};
use crate::service::WorkspaceRuntimeService;

impl WorkspaceRuntimeService {
    pub fn latest_snapshot(
        &self,
        request: LatestSnapshotRequest,
    ) -> Result<ReadonlySnapshotHandle, WorkspaceError> {
        if let Some(hooks) = self.hooks() {
            return (hooks.latest_snapshot)(request);
        }

        ensure_absolute(&request.workspace_root, "workspace_root")?;
        ensure_non_empty(&request.owner_request_id, "owner_request_id")?;

        let snapshot =
            layerstack::service::get_snapshot(&request.workspace_root).map_err(|error| {
                WorkspaceError::SnapshotAcquire {
                    source: error.to_string(),
                }
            })?;
        let generation_key = format!("{}:{}", snapshot.manifest_version, snapshot.root_hash);
        Ok(ReadonlySnapshotHandle {
            view_root: request.workspace_root,
            generation_key,
            snapshot: snapshot.into(),
        })
    }
}

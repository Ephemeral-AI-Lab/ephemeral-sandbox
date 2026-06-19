use std::collections::BTreeMap;

use crate::error::WorkspaceError;
use crate::model::{
    CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind, WorkspaceHandle,
};
use crate::service::support::active_mode_id;
use crate::service::WorkspaceRuntimeService;

impl WorkspaceRuntimeService {
    pub fn capture_changes(
        &self,
        handle: &WorkspaceHandle,
        request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceError> {
        if let Some(hooks) = self.hooks() {
            return (hooks.capture_changes)(handle, request);
        }

        let upperdir = {
            let state = self.lock_state()?;
            let mode_id = active_mode_id(&state, handle)?;
            let mode_handle =
                state
                    .manager
                    .handles
                    .get(&mode_id)
                    .ok_or_else(|| WorkspaceError::NotOpen {
                        owner: handle.owner.clone(),
                    })?;
            mode_handle.dirs.upperdir.clone()
        };
        let captured = crate::overlay::capture::capture_upperdir(&upperdir).map_err(|error| {
            WorkspaceError::Capture {
                message: error.to_string(),
            }
        })?;
        let changed_paths = captured
            .changes
            .iter()
            .map(|change| change.path().as_str().to_owned())
            .collect::<Vec<_>>();
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
        let metadata_path_count = captured
            .changes
            .len()
            .saturating_add(captured.protected_drops.len());
        Ok(CapturedWorkspaceChanges {
            workspace_id: handle.id.clone(),
            base_revision: handle.base_revision.clone(),
            changed_paths,
            changed_path_kinds,
            protected_drops: captured.protected_drops,
            stats: request.include_stats.then_some(captured.stats),
            changes: captured.changes,
            route_stats: layerstack::CaptureRouteStats::default(),
            metadata_path_count,
            spool_dir: None,
        })
    }
}

use std::collections::BTreeMap;

use crate::error::WorkspaceError;
use crate::model::{
    CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind, WorkspaceHandle,
};
use crate::namespace::holder::HolderFinalizationProof;
use crate::service::WorkspaceRuntimeService;

impl WorkspaceRuntimeService {
    pub fn capture_changes(
        &self,
        handle: &WorkspaceHandle,
        request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceError> {
        let _admission = self.admit_work()?;
        if let Some(hooks) = self.hooks() {
            return (hooks.capture_changes)(handle, request);
        }

        let upperdir = {
            let state = self.lock_state()?;
            let session = state
                .manager
                .handles
                .get(&handle.id)
                .ok_or(WorkspaceError::NotOpen)?;
            if session.external_overlay_authority {
                return Err(WorkspaceError::Capture {
                    message:
                        "external MPLA overlays are captured only by the typed storage lifecycle"
                            .to_owned(),
                });
            }
            if !handle.matches_mounted_workspace(session) || !handle.holder_is_live() {
                return Err(WorkspaceError::NotOpen);
            }
            session.dirs.upperdir.clone()
        };
        capture_upperdir(handle, request, &upperdir)
    }

    pub fn capture_changes_after_holder_quiesced(
        &self,
        handle: &WorkspaceHandle,
        proof: &HolderFinalizationProof,
        request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceError> {
        let _admission = self.admit_work()?;
        if let Some(hooks) = self.hooks() {
            return (hooks.capture_changes_after_holder_quiesced)(handle, proof, request);
        }

        let upperdir = {
            let state = self.lock_state()?;
            let session = state
                .manager
                .handles
                .get(&handle.id)
                .ok_or(WorkspaceError::NotOpen)?;
            if session.external_overlay_authority {
                return Err(WorkspaceError::Capture {
                    message:
                        "external MPLA overlays are captured only by the typed storage lifecycle"
                            .to_owned(),
                });
            }
            if !handle.matches_mounted_workspace(session)
                || !handle
                    .holder_registration()
                    .matches_finalization_proof(proof)
            {
                return Err(WorkspaceError::NotOpen);
            }
            session.dirs.upperdir.clone()
        };
        capture_upperdir(handle, request, &upperdir)
    }
}

fn capture_upperdir(
    handle: &WorkspaceHandle,
    request: CaptureChangesRequest,
    upperdir: &std::path::Path,
) -> Result<CapturedWorkspaceChanges, WorkspaceError> {
    let captured = crate::overlay::capture::capture_upperdir(upperdir).map_err(|error| {
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
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest: handle.snapshot.manifest.clone(),
        changed_paths,
        changed_path_kinds,
        protected_drops: captured.protected_drops,
        stats: request.include_stats.then_some(captured.stats),
        changes: captured.changes,
        metadata_path_count,
    })
}

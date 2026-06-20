use std::path::PathBuf;

use crate::workspace_crate::{BaseRevision, WorkspaceHandle, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum WorkspaceRemountState {
    #[default]
    Active,
    RemountPending,
    RemountBlocked,
}

impl WorkspaceRemountState {
    pub(crate) fn is_pending(&self) -> bool {
        matches!(self, Self::RemountPending)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSessionHandler {
    pub workspace_session_id: WorkspaceSessionId,
    pub handle: WorkspaceHandle,
    pub layer_stack_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceSession {
    pub workspace_session_id: WorkspaceSessionId,
    pub handle: WorkspaceHandle,
    pub layer_stack_root: PathBuf,
    pub remount_state: WorkspaceRemountState,
}

impl WorkspaceSession {
    pub(crate) fn from_handle(handle: WorkspaceHandle, layer_stack_root: PathBuf) -> Self {
        Self {
            workspace_session_id: handle.id.clone(),
            layer_stack_root,
            handle,
            remount_state: WorkspaceRemountState::Active,
        }
    }

    pub(crate) fn handler(&self) -> WorkspaceSessionHandler {
        WorkspaceSessionHandler {
            workspace_session_id: self.workspace_session_id.clone(),
            handle: self.handle.clone(),
            layer_stack_root: self.layer_stack_root.clone(),
        }
    }

    pub(crate) fn ensure_remount_not_pending(&self) -> Result<(), WorkspaceSessionError> {
        if self.remount_state.is_pending() {
            return Err(WorkspaceSessionError::RemountAlreadyPending {
                workspace_session_id: self.workspace_session_id.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn active_handle(&self) -> Result<WorkspaceHandle, WorkspaceSessionError> {
        self.ensure_remount_not_pending()?;
        Ok(self.handle.clone())
    }

    pub(crate) fn begin_remount(
        &mut self,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        if self.remount_state.is_pending() {
            return Err(WorkspaceSessionError::RemountAlreadyPending {
                workspace_session_id: self.workspace_session_id.clone(),
            });
        }
        self.remount_state = WorkspaceRemountState::RemountPending;
        Ok(self.handler())
    }

    pub(crate) fn finish_remount(&mut self) -> Result<(), WorkspaceSessionError> {
        if !self.remount_state.is_pending() {
            return Err(WorkspaceSessionError::RemountNotPending {
                workspace_session_id: self.workspace_session_id.clone(),
            });
        }
        self.remount_state = WorkspaceRemountState::Active;
        Ok(())
    }

    pub(crate) fn block_remount(&mut self) -> Result<(), WorkspaceSessionError> {
        if !self.remount_state.is_pending() {
            return Err(WorkspaceSessionError::RemountNotPending {
                workspace_session_id: self.workspace_session_id.clone(),
            });
        }
        self.remount_state = WorkspaceRemountState::RemountBlocked;
        Ok(())
    }

    pub(crate) fn refresh_after_capture(&mut self, base_revision: BaseRevision) {
        self.handle.base_revision = base_revision;
        self.handle.snapshot.manifest_version = self.handle.base_revision.version;
        self.handle.snapshot.root_hash = self.handle.base_revision.root_hash.clone();
    }

    pub(crate) fn refresh_from_handle(
        &mut self,
        handle: WorkspaceHandle,
    ) -> Result<(), WorkspaceSessionError> {
        if handle.id != self.workspace_session_id {
            return Err(WorkspaceSessionError::RemountWorkspaceSessionIdMismatch {
                expected: self.workspace_session_id.clone(),
                actual: handle.id,
            });
        }

        self.handle = handle;
        Ok(())
    }
}

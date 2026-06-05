use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use eos_workspace_api::{
    CommandWorkspacePolicy, FinalizeCommandRequest, PrepareCommandRequest,
    PreparedCommandWorkspace, WorkspaceApiError, WorkspaceCommandOutcome,
};

use super::types::{EphemeralCommandFinalizeContext, EphemeralCommandSessionPort};
use super::{finalize, prepare};
use crate::{
    CallerId, EphemeralRunDirs, EphemeralSnapshot, EphemeralWorkspace, InvocationId, WorkspaceRoot,
};

pub struct EphemeralCommandPolicy<P>
where
    P: EphemeralCommandSessionPort,
{
    port: P,
    state: Mutex<Option<EphemeralCommandWorkspace>>,
}

pub(super) struct EphemeralCommandWorkspace {
    pub(super) caller_id: String,
    pub(super) invocation_id: String,
    pub(super) root: PathBuf,
    pub(super) lease_id: String,
    pub(super) manifest_version: i64,
    pub(super) manifest_root_hash: String,
    pub(super) layer_paths: Vec<PathBuf>,
    pub(super) workspace_root: PathBuf,
    pub(super) dirs: EphemeralRunDirs,
}

impl<P> EphemeralCommandPolicy<P>
where
    P: EphemeralCommandSessionPort,
{
    #[must_use]
    pub fn new(port: P) -> Self {
        Self {
            port,
            state: Mutex::new(None),
        }
    }

    fn finalize_context(
        &self,
        workspace: &EphemeralCommandWorkspace,
    ) -> Result<EphemeralCommandFinalizeContext, WorkspaceApiError> {
        Ok(EphemeralCommandFinalizeContext {
            workspace: EphemeralWorkspace {
                layer_stack_root: WorkspaceRoot(workspace.root.clone()),
                workspace_root: workspace.workspace_root.clone(),
                caller_id: CallerId(workspace.caller_id.clone()),
                invocation_id: InvocationId(workspace.invocation_id.clone()),
                snapshot: EphemeralSnapshot {
                    lease_id: workspace.lease_id.clone(),
                    manifest_version: workspace.manifest_version,
                    manifest_root_hash: workspace.manifest_root_hash.clone(),
                    layer_paths: workspace.layer_paths.clone(),
                },
                dirs: workspace.dirs.clone(),
            },
            base_timings: self.port.base_timings()?,
        })
    }

    fn cleanup_workspace(&self, workspace: EphemeralCommandWorkspace) {
        let _ = std::fs::remove_dir_all(&workspace.dirs.run_dir);
        let _ = self.port.release_snapshot(&workspace.lease_id);
    }
}

impl<P> CommandWorkspacePolicy for EphemeralCommandPolicy<P>
where
    P: EphemeralCommandSessionPort + Send + Sync,
{
    fn prepare_command_workspace(
        &self,
        request: PrepareCommandRequest,
    ) -> Result<PreparedCommandWorkspace, WorkspaceApiError> {
        let prepared = prepare::prepare_command_workspace(&self.port, request)?;
        let previous = {
            let mut state = lock(&self.state);
            state.replace(prepared.workspace)
        };
        if let Some(previous) = previous {
            self.cleanup_workspace(previous);
        }
        Ok(prepared.prepared)
    }

    fn finalize_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
        let workspace = lock(&self.state).take().ok_or_else(|| {
            WorkspaceApiError::new(
                "ephemeral_command_finalize_failed",
                "ephemeral command workspace is not prepared",
            )
        })?;
        let context = match self.finalize_context(&workspace) {
            Ok(context) => context,
            Err(error) => {
                self.cleanup_workspace(workspace);
                return Err(error);
            }
        };
        let outcome = finalize::finalize_command_workspace(&self.port, context, request);
        self.cleanup_workspace(workspace);
        outcome
    }
}

impl<P> Drop for EphemeralCommandPolicy<P>
where
    P: EphemeralCommandSessionPort,
{
    fn drop(&mut self) {
        if let Some(workspace) = self
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = std::fs::remove_dir_all(&workspace.dirs.run_dir);
            let _ = self.port.release_snapshot(&workspace.lease_id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

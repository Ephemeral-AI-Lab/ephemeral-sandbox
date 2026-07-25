use std::path::PathBuf;

use crate::error::WorkspaceError;
use crate::lifecycle::{execute_remount, ReapedSession, RemountOutcome};
use crate::model::WorkspaceSessionId;
use crate::service::support::workspace_error_from_manager_error;
use crate::service::WorkspaceRuntimeService;

impl WorkspaceRuntimeService {
    /// Run the live remount transaction for one session. `Ok(None)` means
    /// the session is gone (the caller's silent skip); every other outcome
    /// follows the C1/C5 rules inside [`RemountOutcome`].
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the runtime state lock is
    /// unavailable or the transaction hits a setup failure before it can
    /// classify.
    pub fn remount_workspace(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        cgroup_procs_path: Option<PathBuf>,
    ) -> Result<Option<RemountOutcome>, WorkspaceError> {
        let _admission = self.admit_work()?;
        if self.hooks().is_some() {
            return Err(WorkspaceError::Setup {
                step: "workspace runtime hooks do not implement remount".to_owned(),
            });
        }
        let (inputs, layer_stack_root) = {
            let state = self.lock_state()?;
            if state
                .manager
                .handle(workspace_session_id)
                .is_some_and(|handle| handle.candidate_admission.is_some())
            {
                return Ok(Some(RemountOutcome::Leased {
                    reason: "unsupported:strict_candidate_generation_is_session_stable".to_owned(),
                }));
            }
            let Some(inputs) = state.manager.remount_snapshot(workspace_session_id) else {
                return Ok(None);
            };
            (inputs, state.layer_stack_root.clone())
        };
        let effect = execute_remount(
            &inputs,
            &layer_stack_root,
            workspace_session_id,
            cgroup_procs_path,
        )
        .map_err(workspace_error_from_manager_error)?;
        let outcome = self
            .lock_state()?
            .manager
            .remount_apply(workspace_session_id, effect);
        Ok(Some(outcome))
    }

    /// Persist the whole handle set once. The post-commit remount sweep defers
    /// per-session persistence to a single call here after all migrations land,
    /// collapsing the sweep's `Θ(M·N)` serialized handle rewrites + `2M` fsyncs
    /// into one `Θ(N)` write. Hook-backed services persist nothing.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the runtime state lock is unavailable or
    /// the handle-file write fails.
    pub fn persist_handles(&self) -> Result<(), WorkspaceError> {
        let _admission = self.admit_work()?;
        if self.hooks().is_some() {
            return Ok(());
        }
        self.lock_state()?
            .manager
            .persist_handles()
            .map_err(workspace_error_from_manager_error)
    }

    /// The authoritative post-remount handle for a live session, or `None`
    /// when the session is gone — the operation layer refreshes its
    /// registry copy from this after a switch.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the runtime state lock is
    /// unavailable.
    pub fn current_handle(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<Option<crate::model::WorkspaceHandle>, WorkspaceError> {
        let _admission = self.admit_work()?;
        if self.hooks().is_some() {
            return Ok(None);
        }
        let state = self.lock_state()?;
        Ok(state
            .manager
            .handle(workspace_session_id)
            .map(crate::model::WorkspaceHandle::from))
    }

    /// Boot reap: destroy every persisted handle's run dir and reset the
    /// handle file — every persisted session is provably dead (PDEATHSIG).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the runtime state lock is
    /// unavailable; hook-backed services reap nothing.
    pub fn reap_persisted_sessions(&self) -> Result<Vec<ReapedSession>, WorkspaceError> {
        let _admission = self.admit_work()?;
        if self.hooks().is_some() {
            return Ok(Vec::new());
        }
        self.lock_state()?
            .manager
            .reap_persisted_handles()
            .map_err(workspace_error_from_manager_error)
    }
}

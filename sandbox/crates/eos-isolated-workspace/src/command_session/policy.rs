use std::sync::{Mutex, MutexGuard, PoisonError};

use eos_workspace_api::{
    CommandWorkspacePolicy, FinalizeCommandRequest, PrepareCommandRequest,
    PreparedCommandWorkspace, WorkspaceApiError, WorkspaceCommandOutcome,
};
use serde_json::Value;

use super::types::IsolatedCommandSessionPort;
use super::{finalize, prepare};

pub struct IsolatedCommandPolicy<P>
where
    P: IsolatedCommandSessionPort,
{
    port: P,
    prepared: Mutex<bool>,
}

impl<P> IsolatedCommandPolicy<P>
where
    P: IsolatedCommandSessionPort,
{
    #[must_use]
    pub fn new(port: P) -> Self {
        Self {
            port,
            prepared: Mutex::new(false),
        }
    }
}

impl<P> CommandWorkspacePolicy for IsolatedCommandPolicy<P>
where
    P: IsolatedCommandSessionPort + Send + Sync,
{
    fn prepare_command_workspace(
        &self,
        request: PrepareCommandRequest,
    ) -> Result<PreparedCommandWorkspace, WorkspaceApiError> {
        let prepared = prepare::prepare_command_workspace(&self.port, request)?;
        *lock(&self.prepared) = true;
        Ok(prepared)
    }

    fn command_session_started(&self, command_session_id: &str, caller_id: &str) {
        self.port
            .command_session_started(command_session_id, caller_id);
    }

    fn command_session_finished(&self, command_session_id: &str, caller_id: &str, status: &str) {
        self.port
            .command_session_finished(command_session_id, caller_id, status);
    }

    fn finalize_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
        let mut prepared = lock(&self.prepared);
        if !*prepared {
            return Err(WorkspaceApiError::new(
                "isolated_command_finalize_failed",
                "isolated command workspace is not prepared",
            ));
        }
        *prepared = false;
        drop(prepared);

        let mut outcome = finalize::finalize_command_workspace(&self.port, request)?;
        let audit = outcome
            .metadata
            .get("audit")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(metadata) = outcome.metadata.as_object_mut() {
            metadata.remove("audit");
        }
        self.port.record_command_audit(merge_changed_paths(
            audit,
            serde_json::json!(outcome.changed_paths),
        ));
        Ok(outcome)
    }
}

fn merge_changed_paths(mut audit: Value, changed_paths: Value) -> Value {
    if let Some(object) = audit.as_object_mut() {
        object.insert("changed_paths".to_owned(), changed_paths);
    }
    audit
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

use crate::id::NamespaceExecutionId;
use crate::status::NamespaceExecutionTerminalStatus;

/// Drives running/terminal lifecycle by id. `begin` stays in the operation layer
/// (it owns the `WorkspaceSessionId`), so the engine needs no workspace knowledge.
pub trait ExecutionObserver: Send + Sync {
    fn on_running(&self, id: &NamespaceExecutionId);
    fn on_terminal(
        &self,
        id: &NamespaceExecutionId,
        status: NamespaceExecutionTerminalStatus,
        exit_code: Option<i64>,
    );
}

#[derive(Debug, Default)]
pub struct NoopObserver;

impl ExecutionObserver for NoopObserver {
    fn on_running(&self, _id: &NamespaceExecutionId) {}

    fn on_terminal(
        &self,
        _id: &NamespaceExecutionId,
        _status: NamespaceExecutionTerminalStatus,
        _exit_code: Option<i64>,
    ) {
    }
}

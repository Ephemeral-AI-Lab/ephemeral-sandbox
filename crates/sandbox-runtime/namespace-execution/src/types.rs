use std::path::PathBuf;

use sandbox_runtime_namespace_process::runner::protocol::NsFds;

use crate::shell::NamespaceExecutionTerminalStatus;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NamespaceExecutionId(pub String);

#[derive(Debug, Clone)]
pub struct NamespaceTarget {
    pub workspace_root: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    pub upperdir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub ns_fds: NsFds,
}

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

//! Concrete private isolated workspace API implementation.

use eos_workspace_contract::{
    EditFileOutcome, EditFileRequest, ReadFileOutcome, ReadFileRequest, WorkspaceApiError,
    WorkspaceFileOps, WorkspaceMode, WorkspaceMutationSink, WorkspaceReadView, WriteFileOutcome,
    WriteFileRequest,
};

/// Concrete isolated workspace capability implementation.
#[derive(Debug, Clone)]
pub struct IsolatedWorkspaceOps<P> {
    ports: P,
}

impl<P> IsolatedWorkspaceOps<P> {
    #[must_use]
    pub fn new(ports: P) -> Self {
        Self { ports }
    }

    #[must_use]
    pub const fn ports(&self) -> &P {
        &self.ports
    }
}

impl<P> WorkspaceFileOps for IsolatedWorkspaceOps<P>
where
    P: WorkspaceReadView + WorkspaceMutationSink,
{
    fn read_file(&self, request: ReadFileRequest) -> Result<ReadFileOutcome, WorkspaceApiError> {
        eos_workspace_contract::file_ops::read_file(self.ports(), WorkspaceMode::Isolated, request)
    }

    fn write_file(&self, request: WriteFileRequest) -> Result<WriteFileOutcome, WorkspaceApiError> {
        eos_workspace_contract::file_ops::write_file(
            self.ports(),
            WorkspaceMode::Isolated,
            "isolated_workspace",
            request,
        )
    }

    fn edit_file(&self, request: EditFileRequest) -> Result<EditFileOutcome, WorkspaceApiError> {
        eos_workspace_contract::file_ops::edit_file(
            self.ports(),
            WorkspaceMode::Isolated,
            "isolated_workspace",
            request,
        )
    }
}

#[cfg(test)]
#[path = "../../tests/isolated/ops_unit.rs"]
mod tests;

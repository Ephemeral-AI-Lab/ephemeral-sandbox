use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceRemountError {
    #[error(transparent)]
    WorkspaceSession(#[from] crate::workspace_session::WorkspaceSessionError),

    #[error(transparent)]
    Command(Box<crate::command::CommandServiceError>),
}

impl From<crate::command::CommandServiceError> for WorkspaceRemountError {
    fn from(error: crate::command::CommandServiceError) -> Self {
        Self::Command(Box::new(error))
    }
}

impl WorkspaceRemountError {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::WorkspaceSession(error) => error.kind(),
            Self::Command(error) => error.kind(),
        }
    }
}

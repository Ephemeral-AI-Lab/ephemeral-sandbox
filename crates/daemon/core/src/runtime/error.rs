//! Daemon error algebra and wire-kind mapping.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DaemonError {
    #[error(transparent)]
    Protocol(#[from] crate::wire::ProtocolError),

    #[error("daemon io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("request exceeds {limit} byte limit")]
    RequestTooLarge { limit: usize },

    #[error("daemon request authentication failed")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("daemon state lock poisoned: {0}")]
    StateLockPoisoned(&'static str),

    #[error("daemon services are not available in this dispatch context")]
    ServicesUnavailable,

    #[error(transparent)]
    LayerStack(#[from] layerstack::LayerStackError),

    #[error(transparent)]
    Commit(#[from] layerstack::CommitError),

    #[error("overlay pipeline failure: {0}")]
    OverlayPipeline(String),

    #[error("plugin ops are forbidden while caller has an isolated workspace")]
    ForbiddenInIsolatedWorkspace,

    #[error(transparent)]
    Isolated(#[from] workspace::IsolatedError),
}

impl DaemonError {
    /// Map this error onto the wire error `kind`.
    #[must_use]
    pub fn wire_kind(&self) -> crate::wire::ErrorKind {
        use crate::wire::ErrorKind;
        match self {
            Self::Protocol(_) => ErrorKind::BadJson,
            Self::InvalidRequest(_) => ErrorKind::InvalidRequest,
            Self::RequestTooLarge { .. } => ErrorKind::RequestTooLarge,
            Self::Unauthorized => ErrorKind::Unauthorized,
            Self::Forbidden(_) => ErrorKind::Forbidden,
            Self::LayerStack(error) if layer_stack_lifecycle_in_progress(error) => {
                ErrorKind::LifecycleInProgress
            }
            Self::Commit(layerstack::CommitError::Storage(error))
                if layer_stack_lifecycle_in_progress(error) =>
            {
                ErrorKind::LifecycleInProgress
            }
            Self::ForbiddenInIsolatedWorkspace => ErrorKind::ForbiddenInIsolatedWorkspace,
            _ => ErrorKind::InternalError,
        }
    }
}

fn layer_stack_lifecycle_in_progress(error: &layerstack::LayerStackError) -> bool {
    matches!(
        error,
        layerstack::LayerStackError::Storage(message)
            if message.contains("blocked by active leases")
    )
}

impl From<plugin::PluginRuntimeError> for DaemonError {
    fn from(err: plugin::PluginRuntimeError) -> Self {
        use plugin::PluginRuntimeError;
        match err {
            PluginRuntimeError::ForbiddenInIsolatedWorkspace => Self::ForbiddenInIsolatedWorkspace,
            PluginRuntimeError::StateLockPoisoned(what) => Self::StateLockPoisoned(what),
            PluginRuntimeError::InvalidRequest(message) => Self::InvalidRequest(message),
            PluginRuntimeError::Io(source) => Self::Io(source),
            PluginRuntimeError::LayerStack(source) => Self::LayerStack(source),
            PluginRuntimeError::PluginDisabled(provider) => {
                Self::Forbidden(format!("plugin provider {provider} is disabled"))
            }
            PluginRuntimeError::PyrightLsp(message) => Self::OverlayPipeline(message),
        }
    }
}

impl From<workspace::LaunchError> for DaemonError {
    fn from(err: workspace::LaunchError) -> Self {
        use workspace::LaunchError;
        match err {
            LaunchError::InvalidRequest(message) => Self::InvalidRequest(message),
            LaunchError::Io(source) => Self::Io(source),
            LaunchError::Failed(message) => Self::OverlayPipeline(message),
        }
    }
}

impl From<operation::checkpoint::CheckpointError> for DaemonError {
    fn from(err: operation::checkpoint::CheckpointError) -> Self {
        use operation::checkpoint::CheckpointError;
        match err {
            CheckpointError::InvalidRequest(message) => Self::InvalidRequest(message),
            CheckpointError::Forbidden(message) => Self::Forbidden(message),
            CheckpointError::OverlayPipeline(message) => Self::OverlayPipeline(message),
            CheckpointError::LayerStack(source) => Self::LayerStack(source),
            CheckpointError::Io(source) => Self::Io(source),
        }
    }
}

//! Daemon error algebra and wire-kind mapping.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DaemonError {
    #[error(transparent)]
    Protocol(#[from] crate::wire::ProtocolError),

    #[error("daemon io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),

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
    LayerStack(#[from] eos_layerstack::LayerStackError),

    #[error(transparent)]
    Commit(#[from] eos_layerstack::CommitError),

    #[error("overlay pipeline failure: {0}")]
    OverlayPipeline(String),

    #[error(transparent)]
    Plugin(#[from] eos_plugin::PluginError),

    #[error(transparent)]
    Isolated(#[from] eos_isolated_workspace::IsolatedError),
}

impl DaemonError {
    /// Map this error onto the wire error `kind`.
    #[must_use]
    pub const fn wire_kind(&self) -> crate::wire::ErrorKind {
        use crate::wire::ErrorKind;
        match self {
            Self::Protocol(_) => ErrorKind::BadJson,
            Self::InvalidEnvelope(_) => ErrorKind::InvalidEnvelope,
            Self::RequestTooLarge { .. } => ErrorKind::RequestTooLarge,
            Self::Unauthorized => ErrorKind::Unauthorized,
            Self::Forbidden(_) => ErrorKind::Forbidden,
            Self::Plugin(eos_plugin::PluginError::ForbiddenInIsolatedWorkspace) => {
                ErrorKind::ForbiddenInIsolatedWorkspace
            }
            _ => ErrorKind::InternalError,
        }
    }
}

impl From<eos_plugin_ops::PluginRuntimeError> for DaemonError {
    fn from(err: eos_plugin_ops::PluginRuntimeError) -> Self {
        use eos_plugin_ops::PluginRuntimeError;
        match err {
            PluginRuntimeError::Plugin(source) => Self::Plugin(source),
            PluginRuntimeError::Ppc(source) => Self::from(source),
            PluginRuntimeError::Launch(source) => Self::from(source),
            PluginRuntimeError::StateLockPoisoned(what) => Self::StateLockPoisoned(what),
            PluginRuntimeError::OverlayPipeline(message) => Self::OverlayPipeline(message),
            PluginRuntimeError::InvalidRequest(message) => Self::InvalidEnvelope(message),
            PluginRuntimeError::Io(source) => Self::Io(source),
            PluginRuntimeError::LayerStack(source) => Self::LayerStack(source),
            PluginRuntimeError::Commit(source) => Self::Commit(source),
        }
    }
}

impl From<eos_plugin_ops::LaunchError> for DaemonError {
    fn from(err: eos_plugin_ops::LaunchError) -> Self {
        use eos_plugin_ops::LaunchError;
        match err {
            LaunchError::InvalidRequest(message) => Self::InvalidEnvelope(message),
            LaunchError::Io(source) => Self::Io(source),
            LaunchError::Failed(message) => Self::OverlayPipeline(message),
        }
    }
}

impl From<eos_plugin_ops::PpcError> for DaemonError {
    fn from(err: eos_plugin_ops::PpcError) -> Self {
        use eos_plugin_ops::PpcError;
        match err {
            PpcError::Plugin(source) => Self::Plugin(source),
            // The plugin channel frames with its own copy of the wire framing
            // (no shared protocol crate); a PPC parse failure re-wraps as a
            // plugin-channel error rather than a daemon-wire `bad_json`.
            PpcError::Protocol(source) => {
                Self::Plugin(eos_plugin::PluginError::Ppc(source.to_string()))
            }
            PpcError::Io(source) => Self::Io(source),
            PpcError::LockPoisoned(what) => Self::StateLockPoisoned(what),
            PpcError::Callback(message) => Self::Plugin(eos_plugin::PluginError::Ppc(message)),
        }
    }
}

impl From<eos_checkpoint::CheckpointError> for DaemonError {
    fn from(err: eos_checkpoint::CheckpointError) -> Self {
        use eos_checkpoint::CheckpointError;
        match err {
            CheckpointError::InvalidEnvelope(message) => Self::InvalidEnvelope(message),
            CheckpointError::Forbidden(message) => Self::Forbidden(message),
            CheckpointError::OverlayPipeline(message) => Self::OverlayPipeline(message),
            CheckpointError::LayerStack(source) => Self::LayerStack(source),
            CheckpointError::Io(source) => Self::Io(source),
        }
    }
}

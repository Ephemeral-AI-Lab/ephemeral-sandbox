//! Daemon error algebra.
//!
//! `thiserror` enum per crate (no `Box<dyn Error>` in the public API). Source
//! conversions use `#[from]`; messages are lowercase with no trailing
//! punctuation. The lower-crate error types fold in via `#[from]` so a handler
//! can `?`-propagate them; the dispatcher maps a [`DaemonError`] onto the wire
//! [`crate::wire::ErrorKind`] error envelope.

use thiserror::Error;

/// Failures surfaced by the daemon server, dispatcher, and the injected port
/// implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DaemonError {
    /// A framed wire message could not be encoded/decoded.
    #[error(transparent)]
    Protocol(#[from] crate::wire::ProtocolError),

    /// A transport / listener I/O operation failed.
    #[error("daemon io error: {0}")]
    Io(#[from] std::io::Error),

    /// The envelope was structurally invalid (missing/empty op, non-object args).
    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),

    /// A request line exceeded [`crate::wire::MAX_REQUEST_BYTES`].
    #[error("request exceeds {limit} byte limit")]
    RequestTooLarge {
        /// The configured per-request byte ceiling.
        limit: usize,
    },

    /// A TCP request's auth token did not match the configured token.
    #[error("daemon request authentication failed")]
    Unauthorized,

    /// A handler/gate policy refusal (e.g. floor-reset env gate not set).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// A process-local daemon state mutex was poisoned.
    #[error("daemon state lock poisoned: {0}")]
    StateLockPoisoned(&'static str),

    /// The layer-stack storage / publish layer failed.
    #[error(transparent)]
    LayerStack(#[from] eos_layerstack::LayerStackError),

    /// The OCC publish path failed.
    #[error(transparent)]
    Commit(#[from] eos_layerstack::CommitError),

    /// The daemon-owned overlay pipeline / dispatch failed.
    #[error("overlay pipeline failure: {0}")]
    OverlayPipeline(String),

    /// The plugin (PPC) dispatch failed.
    #[error(transparent)]
    Plugin(#[from] eos_plugin::PluginError),

    /// The isolated-workspace lifecycle failed.
    #[error(transparent)]
    Isolated(#[from] eos_isolated_workspace::IsolatedError),
}

impl DaemonError {
    /// Map this error onto the wire error `kind`.
    ///
    /// The dispatcher uses this to build the structured error envelope; an
    /// otherwise-unclassified handler failure becomes
    /// [`crate::wire::ErrorKind::InternalError`] with a generated `error_id`.
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

impl From<eos_plugin::host::PpcError> for DaemonError {
    /// Fold a plugin-host PPC / package failure onto the matching daemon
    /// variant. `Plugin` keeps the inner [`eos_plugin::PluginError`] so
    /// `wire_kind` still classifies `ForbiddenInIsolatedWorkspace`; `Callback`
    /// carries an already-formatted message that re-wraps as a PPC error.
    fn from(err: eos_plugin::host::PpcError) -> Self {
        use eos_plugin::host::PpcError;
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

impl From<crate::services::checkpoint::CheckpointError> for DaemonError {
    /// Fold a host checkpoint failure onto the matching daemon variant,
    /// preserving variant identity (so `wire_kind` classifies `Forbidden`
    /// correctly) and the original message text.
    fn from(err: crate::services::checkpoint::CheckpointError) -> Self {
        use crate::services::checkpoint::CheckpointError;
        match err {
            CheckpointError::InvalidEnvelope(message) => Self::InvalidEnvelope(message),
            CheckpointError::Forbidden(message) => Self::Forbidden(message),
            CheckpointError::OverlayPipeline(message) => Self::OverlayPipeline(message),
            CheckpointError::LayerStack(source) => Self::LayerStack(source),
            CheckpointError::Io(source) => Self::Io(source),
        }
    }
}

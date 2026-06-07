//! OCC error type.
//!
//! `thiserror` enum per crate (no `Box<dyn Error>` in the public API). Source
//! conversions use `#[from]`; messages are lowercase with no trailing
//! punctuation.

use eos_protocol::CasError;

/// Errors raised by the OCC publish path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OccError {
    /// The commit queue was closed before the publish could be enqueued.
    #[error("occ commit queue is closed")]
    QueueClosed,

    /// The commit queue worker thread was never started.
    #[error("occ commit queue has not been started")]
    QueueNotStarted,

    /// The commit queue worker thread could not be started.
    #[error("occ commit queue worker failed to start: {0}")]
    WorkerStart(String),

    /// The commit queue worker thread panicked while closing.
    #[error("occ commit queue worker panicked")]
    WorkerPanicked,

    /// A process-local commit queue mutex was poisoned.
    #[error("occ commit queue state lock poisoned: {0}")]
    QueueStatePoisoned(&'static str),

    /// The single-writer reply channel dropped before delivering a result.
    #[error("occ commit reply channel disconnected")]
    ReplyDisconnected,

    /// The layer-stack publisher rejected every CAS retry attempt.
    ///
    /// The publish surfaces a per-path `OccStatus::AbortedVersion` result
    /// rather than looping; this error carries the exhausted attempt count.
    #[error("cas mismatch retry budget exhausted after {attempts} attempts")]
    CasRetryExhausted {
        /// Number of attempts spent before giving up (`MAX_OCC_CAS_RETRIES`).
        attempts: u32,
    },

    /// Route/base-hash preparation failed before enqueueing.
    #[error("occ route preparation failed: {0}")]
    RoutePreparation(String),

    /// A path/hash from `eos-protocol` failed to parse or validate.
    #[error(transparent)]
    Cas(#[from] CasError),
}

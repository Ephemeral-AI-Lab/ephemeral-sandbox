//! The single `thiserror` error enum for the audit side channel.
//!
//! `AuditError` covers the three recoverable failures an audit sink can report:
//! a `JSONL` write IO error, an event serialization error, and the bounded-queue
//! backpressure signal raised by [`BufferedJsonlSink`](crate::BufferedJsonlSink).

/// Errors reported by an [`AuditSink`](crate::AuditSink).
///
/// These are *recoverable* failures surfaced through `Result` without
/// interrupting the emitting domain path. Sinks must not panic.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// Appending the event to a `JSONL` file failed.
    #[error("audit jsonl write failed")]
    Jsonl(#[from] std::io::Error),
    /// Encoding the event to canonical `JSON` failed.
    #[error("audit event serialization failed")]
    Serialize(#[from] serde_json::Error),
    /// The bounded sink queue is full; the event was dropped rather than block
    /// the caller's runtime thread.
    #[error("audit sink queue is full")]
    Backpressure,
}

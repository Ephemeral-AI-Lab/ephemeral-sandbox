//! The [`AuditSink`] write-only seam and the [`NoopAuditSink`].
//!
//! This is the DIP/OCP boundary (anchor §6): high-level emitters depend on the
//! trait; concrete destinations (`JSONL` writer, in-memory test sink, noop) are
//! injected at the composition root behind `Arc<dyn AuditSink>`. The trait is a
//! single-method write surface (ISP) and is deliberately **not** sealed —
//! external sinks are first-class implementors.

use crate::error::AuditError;
use crate::event::AuditEvent;

/// Write-only audit side channel.
///
/// Implementations must not panic; recoverable failures are reported through
/// [`AuditError`]. The event is borrowed, not consumed.
pub trait AuditSink: Send + Sync {
    /// Persist one event.
    ///
    /// # Errors
    /// Returns [`AuditError`] when the sink cannot persist the event (e.g. an
    /// IO failure or a full bounded queue).
    fn publish(&self, event: &AuditEvent) -> Result<(), AuditError>;
}

/// Audit sink used when collection is disabled; every publish is a no-op.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopAuditSink;

impl AuditSink for NoopAuditSink {
    fn publish(&self, _event: &AuditEvent) -> Result<(), AuditError> {
        Ok(())
    }
}

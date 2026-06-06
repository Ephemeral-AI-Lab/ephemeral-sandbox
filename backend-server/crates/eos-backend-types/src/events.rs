//! Persisted milestone event record (`event_log`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use eos_types::{RequestId, UtcDateTime};

/// A persisted milestone event.
///
/// `seq` is a per-request monotonic sequence reserved by the backend event bus
/// (Phase 5). The milestone `kind` vocabulary is also owned by Phase 5; this DTO
/// is the durable record the store persists and the stream replays, with `kind`
/// held as free TEXT to match the `event_log.kind` column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventRecord {
    /// Owning request id.
    pub request_id: RequestId,
    /// Per-request monotonic sequence number.
    pub seq: i64,
    /// Milestone classification (e.g. a run-lifecycle kind, or the gap marker).
    pub kind: String,
    /// Event-specific payload.
    pub payload: serde_json::Value,
    /// When the event was recorded.
    pub created_at: UtcDateTime,
}

/// `kind` value for the dropped-milestone marker. When the bounded event queue
/// overflows, the backend persists/broadcasts this marker so milestone loss is
/// visible in the events API and live stream, never silent.
pub const EVENT_STREAM_GAP: &str = "event_stream_gap";

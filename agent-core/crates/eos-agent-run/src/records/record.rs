use eos_types::{JsonObject, UtcDateTime};
use serde::{Deserialize, Serialize};

/// Byte range produced by a message append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageAppendRange {
    /// Number of message rows appended.
    pub count: usize,
    /// Starting byte offset before the append.
    pub start_byte: u64,
    /// Ending byte offset after the append.
    pub end_byte: u64,
}

/// Raw message-record bytes plus the next tail offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBytes {
    /// Raw JSONL bytes.
    pub bytes: Vec<u8>,
    /// Byte offset after `bytes`.
    pub next_byte_offset: u64,
}

/// One node-local event row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEvent {
    /// Node-local sequence, starting at 1.
    pub seq: u64,
    /// Stable event category.
    pub kind: String,
    /// Small routing/status payload.
    pub payload: JsonObject,
    /// Event creation timestamp.
    pub created_at: UtcDateTime,
}

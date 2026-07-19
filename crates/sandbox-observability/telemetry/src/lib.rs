//! Leaf telemetry crate: one NDJSON record model (`Record::{Span, Event,
//! Sample}`), a single-write `Sink`, a folding `Reader`, and the `Observer` emit
//! API. No storage engine, no runtime/daemon/config dependency — `serde`,
//! `serde_json`, and `thiserror` only.

pub mod collect;
pub mod paths;
pub mod record;

mod lines;
mod observer;
mod reader;
mod sink;

pub use collect::{sample_layerstack, LayerBytes, LayerStackBytes, WalkBudget};
pub use observer::{
    NoopHook, Observer, ObserverConfig, SpanGuard, SpanRegistry, TerminalHook, TraceContext,
};
pub use paths::{ObservabilityPathError, ObservabilityPaths};
pub use reader::{
    EventNode, RawFilter, RawJsonRecords, Reader, ResourceRead, SampleDelta, SpanNode,
    MAX_RESPONSE_BYTES, MAX_RESPONSE_RECORDS,
};
pub use record::{
    Attrs, Event, Record, Sample, Span, SpanStatus, COUNTERS_METRIC_KEY, MAX_LINE_BYTES,
    TRUNCATED_KEY,
};
pub use sink::{Sink, SinkStats, DEFAULT_MAX_DISK_BYTES, MAX_DISK_BYTES};

/// Current unix time in milliseconds — the single clock the emit/read sides
/// self-stamp from; no caller threads a timestamp.
pub(crate) fn unix_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

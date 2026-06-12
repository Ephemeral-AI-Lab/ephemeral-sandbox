#![forbid(unsafe_code)]

pub mod budget;
pub mod codec;
pub mod ids;
pub mod layer;
pub mod record;
pub mod resource_stats;
pub mod spool;
pub mod subscriber;

pub use budget::{BoundedJson, DetailBudget};
pub use codec::{decode_trace_batch, encode_trace_batch, proto, DecodeTraceError, TraceBatch};
pub use ids::{BootId, IdError, RequestId, SpanUid, TraceId};
pub use layer::TraceSpoolLayer;
pub use record::{
    EventRecord, SpanKind, SpanRecord, SpanStatus, SpanSubsystem, TraceKind, TraceLink,
    TraceLinkKind, TraceRecord, WorkspaceRoute,
};
pub use resource_stats::{ResourceStats, ResourceStatsKind, ResourceStatsMeta};
pub use spool::{SpoolInsertOutcome, TraceSpool};

//! Daemon trace assembly: the process-global background spool and request-trace
//! sink, the response sidecar span/event-tree builder with sidecar budget
//! enforcement, and the envelope-meta stamping/rollups rendered from the trace
//! record. This module is a thin facade; the implementation lives in the
//! submodules below.

mod envelope_meta;
mod sidecar;
mod spool;

pub(crate) use sidecar::{attach_request_sidecar, push_transport_failure_from_sidecar};
#[cfg(test)]
pub(crate) use spool::{
    ack_background_export, lease_background_records, now_ms, push_background_record,
    RequestTraceEvent,
};
pub(crate) use spool::{next_connection_id, RequestTraceFacts};

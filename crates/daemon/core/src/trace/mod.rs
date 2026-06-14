//! Daemon trace assembly: the process-global background spool and request-trace
//! sink, the response sidecar span/event-tree builder with sidecar budget
//! enforcement, and the envelope-meta stamping/rollups rendered from the trace
//! record. This module is a thin facade; the implementation lives in the
//! submodules below.

mod envelope_meta;
mod sidecar;
mod spool;

pub(crate) use sidecar::{
    attach_request_sidecar, attach_request_sidecar_with_events, push_transport_failure_from_sidecar,
};
#[cfg(test)]
pub(crate) use spool::now_ms;
pub(crate) use spool::{
    ack_background_export, idle_workspace_evict_record, lease_background_records,
    next_connection_id, push_background_record, RequestTraceEvent, RequestTraceEventSink,
    RequestTraceFacts,
};

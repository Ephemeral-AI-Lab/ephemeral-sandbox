use std::net::SocketAddr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) struct TransportTraceContext {
    pub(super) connection_id: String,
    pub(super) is_tcp: bool,
    pub(super) read_duration_us: u64,
    pub(super) accepted_at_unix_ms: u64,
    pub(super) peer_addr: Option<SocketAddr>,
    pub(super) local_addr: Option<SocketAddr>,
}

pub(super) fn trace_facts(
    transport_context: &TransportTraceContext,
    request_bytes: usize,
    auth_required: bool,
    auth_ok: bool,
    protocol_version: Option<i64>,
) -> crate::trace::RequestTraceFacts {
    crate::trace::RequestTraceFacts {
        connection_id: transport_context.connection_id.clone(),
        accepted_at_unix_ms: transport_context.accepted_at_unix_ms,
        listener_kind: if transport_context.is_tcp {
            "tcp"
        } else {
            "unix"
        },
        peer_addr: transport_context.peer_addr.map(|addr| addr.to_string()),
        local_addr: transport_context.local_addr.map(|addr| addr.to_string()),
        is_tcp: transport_context.is_tcp,
        request_bytes,
        read_duration_us: transport_context.read_duration_us,
        auth_required,
        auth_ok,
        protocol_version,
    }
}

pub(super) fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

pub(super) fn unix_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

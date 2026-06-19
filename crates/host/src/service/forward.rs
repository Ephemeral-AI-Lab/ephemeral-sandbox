use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use protocol::HostGatewayErrorKind;
use serde_json::{json, Value};

mod audit;
mod recovery;
mod response_meta;
mod trace_ingest;

use audit::{persist_response_or_mark_degraded, record_event};
use recovery::run_recovery;
use response_meta::refresh_response_trace_receipt;
pub(crate) use trace_ingest::ingest_and_strip_sidecar;

use crate::daemon_wire::{
    encode_request_with_trace_metadata, response_status, ClientError, ProtocolClient,
    TraceWireContext, TransportAuth,
};
use crate::service::registry::SandboxRecord;
use crate::service::{ForwardTraceContext, HostConfig};
use crate::trace_store::{RequestStartInput, TraceStore, TraceStoreError};
use trace::{sha256_hex, RequestId, TraceId};

#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("trace store unavailable before forwarding: {0}")]
    TraceUnavailable(TraceStoreError),
    #[error("sandbox unavailable: {0}")]
    SandboxUnavailable(String),
    #[error("uncertain outcome: {0}")]
    UncertainOutcome(String),
}

pub(crate) struct ForwardRequestInput<'a> {
    pub(crate) record: Arc<SandboxRecord>,
    pub(crate) config: &'a HostConfig,
    pub(crate) trace_store: &'a Arc<TraceStore>,
    pub(crate) trace_context: ForwardTraceContext,
    pub(crate) mutates_state: bool,
    pub(crate) op: &'a str,
    pub(crate) invocation_id: &'a str,
    pub(crate) args: &'a Value,
}

pub(crate) fn forward_request(input: ForwardRequestInput<'_>) -> Result<Value, ForwardError> {
    let ForwardRequestInput {
        record,
        config,
        trace_store,
        trace_context,
        mutates_state,
        op,
        invocation_id,
        args,
    } = input;
    let record_ref = record.as_ref();
    let trace = TraceWireContext {
        trace_id: trace_context.trace_id.to_string(),
        request_id: trace_context.request_id.to_string(),
        parent_span_id: trace_context.parent_span_id,
        link_hints: Vec::new(),
        capture_budget_version: 1,
    };
    let mut tcp_line = encode_request_with_trace_metadata(
        op,
        invocation_id,
        args,
        TransportAuth::Forward(Some(&record_ref.forward_token)),
        &trace,
    );
    tcp_line.push(b'\n');
    let caller_id = args.get("caller_id").and_then(Value::as_str);
    let decision = trace_store
        .prepare_forward(RequestStartInput {
            sandbox_id: &record.sandbox_id,
            trace_id: trace_context.trace_id.clone(),
            request_id: trace_context.request_id.clone(),
            op,
            caller_id,
            mutates_state,
            args: args.clone(),
        })
        .map_err(ForwardError::TraceUnavailable)?;
    let attempt = ForwardAttempt {
        record: record_ref,
        config,
        trace_store: trace_store.as_ref(),
        trace_id: decision.trace_id,
        request_id: decision.request_id,
        mutates_state,
        tcp_line,
        op,
        invocation_id,
        args,
    };
    if decision.degraded {
        record_event(
            &attempt,
            "host.protocol",
            "trace_degraded",
            json!({"op": op, "mutates_state": mutates_state}),
        );
    }
    for event in trace_context.gateway_events {
        record_event(&attempt, &event.module, &event.event, event.details.clone());
    }
    record_event(
        &attempt,
        "host.protocol",
        "forward_started",
        json!({"op": op, "mutates_state": mutates_state}),
    );
    let mut result = run_recovery(&attempt);
    match &mut result {
        Ok(response) => {
            record_event(
                &attempt,
                "host.protocol",
                "forward_finished",
                json!({"op": op, "status": response_status(response)}),
            );
            refresh_response_trace_receipt(&attempt, response);
        }
        Err(err) => record_event(
            &attempt,
            "host.protocol",
            "forward_failed",
            json!({"op": op, "error_kind": forward_error_kind(err), "message": err.to_string()}),
        ),
    }
    result
}

pub(crate) struct ForwardAttempt<'a> {
    pub(crate) record: &'a SandboxRecord,
    pub(crate) config: &'a HostConfig,
    pub(crate) trace_store: &'a TraceStore,
    pub(crate) trace_id: TraceId,
    pub(crate) request_id: RequestId,
    pub(crate) mutates_state: bool,
    pub(crate) tcp_line: Vec<u8>,
    pub(crate) op: &'a str,
    pub(crate) invocation_id: &'a str,
    pub(crate) args: &'a Value,
}

pub(crate) fn tcp_with_connect_backoff(
    attempt: &ForwardAttempt<'_>,
    endpoint: std::net::SocketAddr,
) -> Result<Value, ClientError> {
    let mut attempt_index = 0_u32;
    let mut last = match tcp_once(attempt, endpoint, attempt_index) {
        Ok(value) => return Ok(value),
        Err(err) if err.is_connect_failure() => err,
        Err(err) => return Err(err),
    };
    for delay_s in connect_retry_delays_s().iter().copied() {
        attempt_index = attempt_index.saturating_add(1);
        record_event(
            attempt,
            "host.transport",
            "retry_scheduled",
            json!({"attempt_index": attempt_index, "delay_ms": duration_ms(Duration::from_secs_f64(delay_s)), "reason": client_error_kind(&last)}),
        );
        std::thread::sleep(Duration::from_secs_f64(delay_s));
        match tcp_once(attempt, endpoint, attempt_index) {
            Ok(value) => return Ok(value),
            Err(err) if err.is_connect_failure() => last = err,
            Err(err) => return Err(err),
        }
    }
    Err(last)
}

pub(crate) fn tcp_once(
    attempt: &ForwardAttempt<'_>,
    endpoint: std::net::SocketAddr,
    attempt_index: u32,
) -> Result<Value, ClientError> {
    let _forward_guard = attempt.record.begin_forward();
    let client = ProtocolClient::new(endpoint, None, attempt.config.request_timeout);
    record_event(
        attempt,
        "host.transport",
        "connect_started",
        json!({
            "sandbox_id": attempt.record.sandbox_id,
            "endpoint": endpoint.to_string(),
            "resolved_addr": endpoint.to_string(),
            "attempt_index": attempt_index,
            "timeout_ms": duration_ms(attempt.config.request_timeout),
        }),
    );
    let started = Instant::now();
    let mut response = match client.request_raw_observed(&attempt.tcp_line) {
        Ok(response) => response,
        Err(err) => {
            record_client_error(attempt, endpoint, attempt_index, started, &err);
            return Err(err);
        }
    };
    let elapsed = elapsed_us(started);
    record_event(
        attempt,
        "host.transport",
        "connect_finished",
        json!({
            "sandbox_id": attempt.record.sandbox_id,
            "endpoint": endpoint.to_string(),
            "resolved_addr": endpoint.to_string(),
            "attempt_index": attempt_index,
            "connect_duration_us": elapsed,
        }),
    );
    record_event(
        attempt,
        "host.transport",
        "request_written",
        json!({
            "request_bytes": attempt.tcp_line.len(),
            "protocol_version": crate::daemon_wire::DAEMON_PROTOCOL_VERSION,
            "auth_token_present": true,
            "write_duration_us": elapsed,
        }),
    );
    let sidecar = ingest_and_strip_sidecar(attempt, &mut response.value);
    record_event(
        attempt,
        "host.transport",
        "response_read",
        json!({
            "response_bytes": response.raw_bytes.len(),
            "read_duration_us": elapsed,
            "response_digest": sha256_hex(&response.raw_bytes),
            "sidecar_present": sidecar.present,
            "sidecar_ingested": sidecar.ingested,
            "sidecar_degraded": sidecar.degraded,
        }),
    );
    persist_response_or_mark_degraded(
        attempt,
        &mut response.value,
        &response.raw_bytes,
        elapsed / 1000,
    )?;
    Ok(response.value)
}

pub(crate) fn record_client_error(
    attempt: &ForwardAttempt<'_>,
    endpoint: SocketAddr,
    attempt_index: u32,
    started: Instant,
    error: &ClientError,
) {
    let details = json!({
        "sandbox_id": attempt.record.sandbox_id,
        "endpoint": endpoint.to_string(),
        "resolved_addr": endpoint.to_string(),
        "attempt_index": attempt_index,
        "error_kind": client_error_kind(error),
        "duration_us": elapsed_us(started),
        "message": error.to_string(),
    });
    let mut details = match details {
        Value::Object(details) => details,
        _ => unreachable!("json object"),
    };
    let (event, duration_field) = match error {
        ClientError::Connect { source, .. } if source.kind() == std::io::ErrorKind::TimedOut => {
            ("connect_timeout", "connect_duration_us")
        }
        ClientError::Connect { .. } => ("connect_failed", "connect_duration_us"),
        ClientError::Write(_) => ("write_failed", "write_duration_us"),
        ClientError::EmptyResponse => ("empty_response", "read_duration_us"),
        ClientError::ResponseTooLarge { .. } => ("response_too_large", "read_duration_us"),
        ClientError::Decode { .. } => ("decode_failed", "read_duration_us"),
        ClientError::Read(_) | ClientError::Io(_) => ("read_failed", "read_duration_us"),
    };
    details.insert(duration_field.to_owned(), json!(elapsed_us(started)));
    record_event(attempt, "host.transport", event, Value::Object(details));
}

pub(crate) fn record_endpoint_refreshed(
    attempt: &ForwardAttempt<'_>,
    old_endpoint: SocketAddr,
    new_endpoint: SocketAddr,
) {
    record_event(
        attempt,
        "host.transport",
        "endpoint_refreshed",
        json!({"old_endpoint": old_endpoint.to_string(), "new_endpoint": new_endpoint.to_string()}),
    );
}

fn client_error_kind(error: &ClientError) -> &'static str {
    match error {
        ClientError::Connect { source, .. } if source.kind() == std::io::ErrorKind::TimedOut => {
            "connect_timeout"
        }
        ClientError::Connect { .. } => "connect_failed",
        ClientError::Io(_) => "transport_io",
        ClientError::Write(_) => "write_failed",
        ClientError::Read(source) if source.kind() == std::io::ErrorKind::TimedOut => {
            "read_timeout"
        }
        ClientError::Read(_) => "read_failed",
        ClientError::EmptyResponse => "empty_response",
        ClientError::ResponseTooLarge { .. } => "response_too_large",
        ClientError::Decode { .. } => "decode_failed",
    }
}

fn forward_error_kind(error: &ForwardError) -> &'static str {
    match error {
        ForwardError::TraceUnavailable(_) => HostGatewayErrorKind::TraceUnavailable.as_str(),
        ForwardError::SandboxUnavailable(_) => HostGatewayErrorKind::SandboxUnavailable.as_str(),
        ForwardError::UncertainOutcome(_) => HostGatewayErrorKind::UncertainOutcome.as_str(),
    }
}

fn retry_attempt_index() -> u32 {
    u32::try_from(connect_retry_delays_s().len()).unwrap_or(u32::MAX)
}

#[cfg(not(test))]
fn connect_retry_delays_s() -> &'static [f64] {
    &crate::daemon_wire::CONNECT_RETRY_DELAYS_S
}

#[cfg(test)]
fn connect_retry_delays_s() -> &'static [f64] {
    &[0.0, 0.0]
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

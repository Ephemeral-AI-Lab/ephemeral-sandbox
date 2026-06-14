use std::time::Instant;

use serde_json::{json, Value};

use crate::container::DaemonContainer;
use crate::daemon_wire::{
    encode_request_with_forward_metadata, encode_request_with_trace_metadata, response_is_accepted,
    ProtocolClient, TraceWireContext, TransportAuth, HEARTBEAT_OP,
};

use super::audit::{persist_response_or_mark_degraded, record_event, record_missing};
use super::trace_ingest::ingest_and_strip_sidecar;
use super::{
    client_error_kind, elapsed_us, retry_attempt_index, tcp_once, tcp_with_connect_backoff,
    ForwardAttempt, ForwardError,
};
use crate::service::registry::resolve_endpoint;

pub(super) fn run_recovery(attempt: &ForwardAttempt<'_>) -> Result<Value, ForwardError> {
    let unavailable = |context: &str, err: &dyn std::fmt::Display| {
        ForwardError::SandboxUnavailable(format!(
            "{} ({context}): {err}",
            attempt.record.sandbox_id
        ))
    };

    let endpoint = match crate::service::registry::cached_or_resolve_endpoint(attempt.record) {
        Ok(addr) => addr,
        Err(err) => {
            record_event(
                attempt,
                "host.transport",
                "endpoint_refresh_failed",
                json!({"reason": "resolve endpoint", "error": err.to_string()}),
            );
            return fallback_chain(attempt, &unavailable("resolve endpoint", &err));
        }
    };
    match tcp_with_connect_backoff(attempt, endpoint) {
        Ok(value) => Ok(value),
        Err(err) if err.is_connect_failure() => match resolve_endpoint(attempt.record) {
            Ok(addr) => {
                super::record_endpoint_refreshed(attempt, endpoint, addr);
                match tcp_once(attempt, addr, retry_attempt_index()) {
                    Ok(value) => Ok(value),
                    Err(err) => {
                        fallback_chain(attempt, &unavailable("retry after re-resolve", &err))
                    }
                }
            }
            Err(err) => fallback_chain(attempt, &unavailable("re-resolve endpoint", &err)),
        },
        Err(err) => {
            if attempt.mutates_state {
                restore_if_unreachable(attempt);
                record_missing(
                    attempt,
                    "uncertain",
                    client_error_kind(&err),
                    &format!("delivery-ambiguous daemon transport failure: {err}"),
                );
                record_event(
                    attempt,
                    "host.protocol",
                    "uncertain_outcome",
                    json!({"error_kind": client_error_kind(&err), "message": err.to_string()}),
                );
                return Err(ForwardError::UncertainOutcome(format!(
                    "{}: {err}",
                    attempt.record.sandbox_id
                )));
            }
            fallback_chain(attempt, &unavailable("tcp request", &err))
        }
    }
}

fn restore_if_unreachable(attempt: &ForwardAttempt<'_>) {
    let probe = resolve_endpoint(attempt.record).ok().and_then(|endpoint| {
        let _forward_guard = attempt.record.begin_forward();
        let client = ProtocolClient::new(endpoint, None, std::time::Duration::from_secs(2));
        let mut line = encode_request_with_forward_metadata(
            HEARTBEAT_OP,
            "recovery-probe",
            &Value::Object(serde_json::Map::new()),
            Some(&attempt.record.forward_token),
        );
        line.push(b'\n');
        client.request_raw(&line).ok()
    });
    if probe.is_some_and(|resp| response_is_accepted(&resp)) {
        return;
    }
    let _ = respawn_and_gate_traced(attempt);
}

fn fallback_chain(
    attempt: &ForwardAttempt<'_>,
    failure: &ForwardError,
) -> Result<Value, ForwardError> {
    record_event(
        attempt,
        "host.transport",
        "fallback_chain_started",
        json!({"sandbox_id": attempt.record.sandbox_id, "reason": failure.to_string()}),
    );
    if let Ok(value) = exec_thin_client(attempt) {
        return Ok(value);
    }
    respawn_and_gate_traced(attempt).map_err(|err| {
        let message = format!("{failure}; respawn failed: {err:#}");
        record_missing(attempt, "error", "sandbox_unavailable", &message);
        ForwardError::SandboxUnavailable(message)
    })?;
    if attempt.mutates_state {
        record_missing(
            attempt,
            "uncertain",
            "uncertain_outcome",
            "daemon respawned after a delivery-ambiguous failure",
        );
        return Err(ForwardError::UncertainOutcome(format!(
            "{}: daemon respawned after a delivery-ambiguous failure; the original outcome is unknowable",
            attempt.record.sandbox_id
        )));
    }
    let endpoint = resolve_endpoint(attempt.record).map_err(|err| {
        ForwardError::SandboxUnavailable(format!("resolve after respawn: {err:#}"))
    })?;
    tcp_once(attempt, endpoint, retry_attempt_index()).map_err(|err| {
        let message = format!("replay after respawn: {err}");
        record_missing(attempt, "error", client_error_kind(&err), &message);
        ForwardError::SandboxUnavailable(message)
    })
}

fn exec_thin_client(attempt: &ForwardAttempt<'_>) -> anyhow::Result<Value> {
    let _forward_guard = attempt.record.begin_forward();
    let container = handle(attempt);
    let socket = attempt
        .config
        .remote_daemon_dir
        .join("runtime.sock")
        .to_string_lossy()
        .into_owned();
    let eosd = attempt
        .config
        .remote_eosd_path
        .to_string_lossy()
        .into_owned();
    let trace = TraceWireContext {
        trace_id: attempt.trace_id.to_string(),
        request_id: attempt.request_id.to_string(),
        parent_span_id: None,
        link_hints: Vec::new(),
        capture_budget_version: 1,
    };
    let payload = String::from_utf8(encode_request_with_trace_metadata(
        attempt.op,
        attempt.invocation_id,
        attempt.args,
        TransportAuth::None,
        &trace,
    ))?;
    record_event(
        attempt,
        "host.transport",
        "exec_client_started",
        json!({
            "sandbox_id": attempt.record.sandbox_id,
            "container": attempt.record.container,
            "remote_socket_path": socket,
            "mutates_state": attempt.mutates_state,
        }),
    );
    let started = Instant::now();
    let stdout = match container.exec(&[&eosd, "daemon", "--client", &socket, &payload]) {
        Ok(stdout) => stdout,
        Err(err) => {
            record_event(
                attempt,
                "host.transport",
                "exec_client_failed",
                json!({"duration_us": elapsed_us(started), "error_kind": "exec_failed", "message": err.to_string()}),
            );
            return Err(err);
        }
    };
    let mut value = serde_json::from_str(stdout.trim())?;
    let sidecar = ingest_and_strip_sidecar(attempt, &mut value);
    record_event(
        attempt,
        "host.transport",
        "exec_client_finished",
        json!({
            "duration_us": elapsed_us(started),
            "sidecar_present": sidecar.present,
            "sidecar_ingested": sidecar.ingested,
            "sidecar_degraded": sidecar.degraded,
        }),
    );
    if let Err(err) = persist_response_or_mark_degraded(
        attempt,
        &mut value,
        stdout.as_bytes(),
        elapsed_us(started) / 1000,
    ) {
        return Err(anyhow::anyhow!(err));
    }
    Ok(value)
}

fn respawn_and_gate_traced(attempt: &ForwardAttempt<'_>) -> anyhow::Result<()> {
    let daemon = attempt.config.daemon_spec(attempt.record.tcp_port);
    record_event(
        attempt,
        "host.transport",
        "daemon_respawn_wait_started",
        json!({"sandbox_id": attempt.record.sandbox_id, "container": attempt.record.container}),
    );
    let _respawn_guard = attempt.record.begin_respawn();
    record_event(
        attempt,
        "host.transport",
        "daemon_respawn_started",
        json!({"sandbox_id": attempt.record.sandbox_id, "container": attempt.record.container}),
    );
    let started = Instant::now();
    match handle(attempt).restart_daemon(&daemon) {
        Ok(()) => {
            record_event(
                attempt,
                "host.transport",
                "daemon_respawn_finished",
                json!({"duration_us": elapsed_us(started)}),
            );
            Ok(())
        }
        Err(err) => {
            record_event(
                attempt,
                "host.transport",
                "daemon_respawn_failed",
                json!({"duration_us": elapsed_us(started), "error_kind": "respawn_failed", "message": err.to_string()}),
            );
            Err(err)
        }
    }
}

fn handle(attempt: &ForwardAttempt<'_>) -> DaemonContainer {
    DaemonContainer::for_engine(
        attempt.record.container.clone(),
        attempt.record.token.clone(),
        attempt.record.forward_token.clone(),
        &attempt.config.daemon_spec(attempt.record.tcp_port),
        attempt.record.cached_endpoint(),
    )
}

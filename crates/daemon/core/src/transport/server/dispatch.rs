use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};

use super::trace_context::{trace_facts, TransportTraceContext};
use super::DaemonServer;
use crate::error::DaemonError;
use crate::wire::{decode_value, ErrorKind, Request, RequestTraceContext, WireMessage};
use crate::DispatchContext;
use protocol::catalog::{BuiltinOp, OpVisibility};

impl DaemonServer {
    pub(super) async fn dispatch_bytes(
        &self,
        bytes: Vec<u8>,
        transport_context: TransportTraceContext,
    ) -> serde_json::Value {
        let request_bytes = bytes.len();
        let auth_required = self.tcp_auth_required(transport_context.is_tcp);
        let parse_error_facts = trace_facts(
            &transport_context,
            request_bytes,
            auth_required,
            false,
            None,
        );
        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                return crate::trace::attach_request_sidecar(
                    crate::dispatcher::error_response(
                        ErrorKind::BadJson,
                        crate::wire::ProtocolError::from(err).to_string(),
                        serde_json::json!({}),
                    ),
                    None,
                    "daemon.transport.decode",
                    &parse_error_facts,
                );
            }
        };
        let trace = value
            .get("trace")
            .cloned()
            .and_then(|value| serde_json::from_value::<RequestTraceContext>(value).ok());
        let value = if transport_context.is_tcp {
            match self.strip_tcp_auth(value) {
                Ok(authenticated) => {
                    if let Err(err) =
                        enforce_tcp_visibility(&authenticated.value, authenticated.authority)
                    {
                        let facts = trace_facts(
                            &transport_context,
                            request_bytes,
                            auth_required,
                            true,
                            None,
                        );
                        let response = crate::dispatcher::error_response(
                            err.wire_kind(),
                            err.to_string(),
                            serde_json::json!({}),
                        );
                        return crate::trace::attach_request_sidecar(
                            response,
                            trace.as_ref(),
                            "daemon.transport.visibility",
                            &facts,
                        );
                    }
                    authenticated.value
                }
                Err(err) => {
                    let facts = trace_facts(
                        &transport_context,
                        request_bytes,
                        auth_required,
                        false,
                        None,
                    );
                    let response = crate::dispatcher::error_response(
                        err.wire_kind(),
                        err.to_string(),
                        serde_json::json!({}),
                    );
                    return crate::trace::attach_request_sidecar(
                        response,
                        trace.as_ref(),
                        "daemon.transport.auth",
                        &facts,
                    );
                }
            }
        } else {
            value
        };
        let protocol_version_value = value
            .get("args")
            .and_then(|args| args.get(crate::wire::DAEMON_PROTOCOL_FIELD));
        let protocol_version = protocol_version_value.and_then(serde_json::Value::as_i64);
        let facts = trace_facts(
            &transport_context,
            request_bytes,
            auth_required,
            true,
            protocol_version,
        );
        if let Some(response) = protocol_version_error(protocol_version_value) {
            return crate::trace::attach_request_sidecar(
                response,
                trace.as_ref(),
                "daemon.transport.version",
                &facts,
            );
        }
        match decode_value(value) {
            Ok(WireMessage::Request(request)) => self.dispatch_request(request, trace, facts).await,
            Ok(_) => crate::trace::attach_request_sidecar(
                crate::dispatcher::error_response(
                    ErrorKind::InvalidRequest,
                    "request must include op, invocation_id, and args",
                    serde_json::json!({}),
                ),
                trace.as_ref(),
                "daemon.transport.decode",
                &facts,
            ),
            Err(err) => crate::trace::attach_request_sidecar(
                crate::dispatcher::error_response(
                    ErrorKind::BadJson,
                    err.to_string(),
                    serde_json::json!({}),
                ),
                trace.as_ref(),
                "daemon.transport.decode",
                &facts,
            ),
        }
    }

    async fn dispatch_request(
        &self,
        request: Request,
        trace: Option<RequestTraceContext>,
        facts: crate::trace::RequestTraceFacts,
    ) -> serde_json::Value {
        let invocation_id = request.invocation_id.clone();
        let caller_id = trimmed_string(&request.args, "caller_id");
        let background = request
            .args
            .get("background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let op = request.op.clone();
        let registry = Arc::clone(&self.invocation_registry);
        let task_registry = Arc::clone(&registry);
        let task_services = Arc::clone(&self.services);
        let file_limits = self.file_limits;
        let trace_events = crate::trace::RequestTraceEventSink::default();
        let task_trace_events = trace_events.clone();
        let task_trace_identity = trace.clone();
        let (start_tx, start_rx) = std_mpsc::channel::<()>();
        let task_started = Arc::new(AtomicBool::new(false));
        let registered_started = Arc::clone(&task_started);
        let task = tokio::task::spawn_blocking(move || {
            let _ = start_rx.recv();
            task_started.store(true, Ordering::SeqCst);
            let mut context =
                DispatchContext::with_runtime_config(&task_services, &task_registry, file_limits)
                    .with_trace_events(task_trace_events);
            if let Some(trace) = task_trace_identity {
                context = context.with_trace_identity(trace.trace_id, trace.request_id);
            }
            crate::dispatcher::dispatch_with_context(&request, context)
        });
        registry.register_blocking(
            &invocation_id,
            task.abort_handle(),
            registered_started,
            &caller_id,
            background,
        );
        let _ = start_tx.send(());
        let response = match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => crate::dispatcher::error_response(
                ErrorKind::InternalError,
                "daemon invocation cancelled",
                serde_json::json!({"op": op}),
            ),
            Err(err) => crate::dispatcher::error_response(
                ErrorKind::InternalError,
                format!("daemon invocation failed: {err}"),
                serde_json::json!({"op": op}),
            ),
        };
        registry.deregister(&invocation_id);
        let request_events = trace_events.drain();
        crate::trace::attach_request_sidecar_with_events(
            response,
            trace.as_ref(),
            &op,
            &facts,
            &request_events,
        )
    }

    pub(super) fn tcp_auth_required(&self, is_tcp: bool) -> bool {
        is_tcp
            && (configured_token(self.config.auth_token.as_deref()).is_some()
                || configured_token(self.config.forward_auth_token.as_deref()).is_some())
    }

    fn strip_tcp_auth(
        &self,
        mut value: serde_json::Value,
    ) -> Result<AuthenticatedTcpRequest, DaemonError> {
        let expected_forward = configured_token(self.config.forward_auth_token.as_deref());
        let expected_raw = configured_token(self.config.auth_token.as_deref());
        let forward_token = value
            .as_object_mut()
            .and_then(|object| object.remove(crate::wire::DAEMON_FORWARD_AUTH_FIELD))
            .and_then(|value| value.as_str().map(str::to_owned));
        let raw_token = value
            .as_object_mut()
            .and_then(|object| object.remove(crate::wire::DAEMON_AUTH_FIELD))
            .and_then(|value| value.as_str().map(str::to_owned));

        if let Some(expected) = expected_forward {
            if forward_token.as_deref() == Some(expected) {
                return Ok(AuthenticatedTcpRequest {
                    value,
                    authority: TcpAuthority::HostForward,
                });
            }
            if forward_token.is_some() {
                return Err(DaemonError::Unauthorized);
            }
        }

        if let Some(expected) = expected_raw {
            if raw_token.as_deref() != Some(expected) {
                return Err(DaemonError::Unauthorized);
            }
            return Ok(AuthenticatedTcpRequest {
                value,
                authority: TcpAuthority::Raw,
            });
        }

        if expected_forward.is_some() {
            return Err(DaemonError::Unauthorized);
        }
        Ok(AuthenticatedTcpRequest {
            value,
            authority: TcpAuthority::Raw,
        })
    }
}

struct AuthenticatedTcpRequest {
    value: serde_json::Value,
    authority: TcpAuthority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TcpAuthority {
    Raw,
    HostForward,
}

fn configured_token(token: Option<&str>) -> Option<&str> {
    token.filter(|token| !token.is_empty())
}

fn enforce_tcp_visibility(
    value: &serde_json::Value,
    authority: TcpAuthority,
) -> Result<(), DaemonError> {
    if authority == TcpAuthority::HostForward {
        return Ok(());
    }
    let Some(op) = value.get("op").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let visibility = BuiltinOp::from_op_name(op).map(|op| op.contract().visibility);
    if visibility.is_some_and(|visibility| visibility != OpVisibility::Public) {
        return Err(DaemonError::Forbidden(format!(
            "raw daemon TCP may not invoke non-public op {op}"
        )));
    }
    Ok(())
}

/// Transport-level caller extraction for in-flight registry keys; runs before
/// any operation parse, so it deliberately applies no default-caller fallback.
fn trimmed_string(args: &serde_json::Value, key: &str) -> String {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn protocol_version_error(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let Some(value) = value else {
        return Some(crate::dispatcher::error_response(
            ErrorKind::InvalidRequest,
            "daemon protocol version is required",
            serde_json::json!({
                "expected": crate::wire::DAEMON_PROTOCOL_VERSION,
                "found": serde_json::Value::Null,
            }),
        ));
    };
    match value.as_i64() {
        Some(crate::wire::DAEMON_PROTOCOL_VERSION) => None,
        Some(found) => Some(crate::dispatcher::error_response(
            ErrorKind::InvalidRequest,
            "unsupported daemon protocol version",
            serde_json::json!({
                "expected": crate::wire::DAEMON_PROTOCOL_VERSION,
                "found": found,
            }),
        )),
        None => Some(crate::dispatcher::error_response(
            ErrorKind::InvalidRequest,
            "daemon protocol version must be an integer",
            serde_json::json!({
                "expected": crate::wire::DAEMON_PROTOCOL_VERSION,
                "found": value,
            }),
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::protocol_version_error;

    #[test]
    fn protocol_version_accepts_supported_version() {
        assert!(
            protocol_version_error(Some(&json!(crate::wire::DAEMON_PROTOCOL_VERSION))).is_none()
        );
    }

    #[test]
    fn protocol_version_rejects_absent_version() {
        let response = protocol_version_error(None).expect("error response");

        assert_eq!(response["status"], "error");
        assert_eq!(response["error"]["kind"], "invalid_request");
        assert_eq!(
            response["error"]["details"]["fields"]["expected"],
            json!(crate::wire::DAEMON_PROTOCOL_VERSION)
        );
        assert_eq!(response["error"]["details"]["fields"]["found"], json!(null));
    }

    #[test]
    fn protocol_version_rejects_unsupported_version() {
        let response = protocol_version_error(Some(&json!(999))).expect("error response");

        assert_eq!(response["status"], "error");
        assert_eq!(response["error"]["kind"], "invalid_request");
        assert_eq!(
            response["error"]["details"]["fields"]["expected"],
            json!(crate::wire::DAEMON_PROTOCOL_VERSION)
        );
        assert_eq!(response["error"]["details"]["fields"]["found"], json!(999));
    }

    #[test]
    fn protocol_version_rejects_non_integer_version() {
        let response = protocol_version_error(Some(&json!("1"))).expect("error response");

        assert_eq!(response["status"], "error");
        assert_eq!(response["error"]["kind"], "invalid_request");
        assert_eq!(response["error"]["details"]["fields"]["found"], json!("1"));
    }
}

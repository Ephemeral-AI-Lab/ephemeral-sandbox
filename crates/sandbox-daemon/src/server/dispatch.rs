use std::sync::Arc;

use super::SandboxDaemonServer;
use crate::server::error::SandboxDaemonError;
use crate::timing;
use sandbox_protocol::{decode_request_value, error_kind, Request, DAEMON_AUTH_FIELD};
use serde_json::{Map, Value};

pub(crate) const PRIVATE_OBSERVABILITY_SNAPSHOT_OP: &str = "get_observability_snapshot";
pub(crate) const PRIVATE_DAEMON_READY_OP: &str = "sandbox_daemon_ready";
const DAEMON_NAME: &str = "sandbox-daemon";

impl SandboxDaemonServer {
    pub(crate) async fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> serde_json::Value {
        let parse_started = std::time::Instant::now();
        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                return super::error_response(
                    error_kind::BAD_JSON,
                    format!("bad json: {err}"),
                    serde_json::json!({}),
                );
            }
        };
        timing::duration("daemon.parse_json", parse_started);
        let auth_started = std::time::Instant::now();
        let value = if is_tcp {
            match strip_tcp_auth(self.config.auth_token.as_deref(), value) {
                Ok(authenticated) => authenticated,
                Err(err) => {
                    return super::error_response(
                        err.response_kind(),
                        err.to_string(),
                        serde_json::json!({}),
                    );
                }
            }
        } else {
            value
        };
        timing::duration("daemon.auth", auth_started);
        let decode_started = std::time::Instant::now();
        match decode_request(value) {
            Ok(request) => {
                timing::duration("daemon.decode_request", decode_started);
                self.dispatch_request(request).await
            }
            Err(response) => response,
        }
    }

    async fn dispatch_request(&self, request: Request) -> serde_json::Value {
        if let Err(response) = validate_daemon_scope(&request) {
            return response;
        }
        if request.op == PRIVATE_DAEMON_READY_OP {
            return sandbox_daemon_ready_response(self.config.sandbox_id.as_deref(), &request);
        }
        if request.op == PRIVATE_OBSERVABILITY_SNAPSHOT_OP {
            return self.dispatch_private_observability_snapshot(request).await;
        }
        let operations = Arc::clone(&self.operations);
        let runtime_started = std::time::Instant::now();
        let task = tokio::task::spawn_blocking(move || {
            sandbox_runtime::dispatch_operation(&operations, &request).into_json_value()
        });
        match task.await {
            Ok(response) => {
                timing::duration("daemon.runtime_dispatch", runtime_started);
                self.trigger_observability_collection();
                response
            }
            Err(err) if err.is_cancelled() => super::error_response(
                error_kind::INTERNAL_ERROR,
                "daemon request cancelled",
                serde_json::json!({}),
            ),
            Err(err) => super::error_response(
                error_kind::INTERNAL_ERROR,
                format!("daemon request failed: {err}"),
                serde_json::json!({}),
            ),
        }
    }

    async fn dispatch_private_observability_snapshot(&self, request: Request) -> Value {
        let Some(observability) = self.observability.clone() else {
            return super::error_response(
                error_kind::INTERNAL_ERROR,
                "daemon observability is not configured",
                serde_json::json!({}),
            );
        };
        let task = tokio::task::spawn_blocking(move || {
            observability
                .observability_snapshot_response(&request)
                .into_json_value()
        });
        match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => super::error_response(
                error_kind::INTERNAL_ERROR,
                "daemon observability snapshot request cancelled",
                serde_json::json!({}),
            ),
            Err(err) => super::error_response(
                error_kind::INTERNAL_ERROR,
                format!("daemon observability snapshot request failed: {err}"),
                serde_json::json!({}),
            ),
        }
    }
}

/// Strip and verify the TCP-only daemon auth token. When a token is configured,
/// a request is accepted only if it carries the matching token, which is removed
/// before decode/dispatch. With no configured token the value passes through.
pub(crate) fn strip_tcp_auth(
    expected_token: Option<&str>,
    mut value: Value,
) -> Result<Value, SandboxDaemonError> {
    let expected_raw = configured_token(expected_token);
    if expected_raw.is_some() {
        let raw_token = match value.as_object_mut() {
            Some(object) => remove_token(object, DAEMON_AUTH_FIELD, expected_raw),
            None => TokenMatch::Missing,
        };
        if raw_token != TokenMatch::Matches {
            return Err(SandboxDaemonError::Unauthorized);
        }
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenMatch {
    Missing,
    Mismatch,
    Matches,
}

fn remove_token(
    object: &mut Map<String, Value>,
    field: &str,
    expected: Option<&str>,
) -> TokenMatch {
    let Some(Value::String(token)) = object.remove(field) else {
        return TokenMatch::Missing;
    };
    if expected == Some(token.as_str()) {
        TokenMatch::Matches
    } else {
        TokenMatch::Mismatch
    }
}

fn configured_token(token: Option<&str>) -> Option<&str> {
    token.filter(|token| !token.is_empty())
}

pub(crate) fn decode_request(value: Value) -> Result<Request, Value> {
    decode_request_value(value)
        .map_err(|err| super::error_response(err.kind(), err.message(), serde_json::json!({})))
}

/// Build the private readiness response. Proves the daemon decoded the request,
/// accepted the sandbox scope, and agrees with the expected sandbox id. When a
/// sandbox id is configured it must match the request's scope; otherwise the
/// request's scope id is echoed back.
pub(crate) fn sandbox_daemon_ready_response(
    configured_sandbox_id: Option<&str>,
    request: &Request,
) -> Value {
    let Some(requested) = request.scope.sandbox_id() else {
        return super::error_response(
            error_kind::INVALID_REQUEST,
            "sandbox_daemon_ready requires sandbox scope",
            serde_json::json!({}),
        );
    };
    let sandbox_id = match configured_sandbox_id {
        Some(configured) if configured != requested => {
            return super::error_response(
                error_kind::INVALID_REQUEST,
                format!(
                    "sandbox id mismatch: daemon is configured for {configured}, request targeted {requested}"
                ),
                serde_json::json!({}),
            );
        }
        Some(configured) => configured,
        None => requested,
    };
    serde_json::json!({
        "status": "ready",
        "sandbox_id": sandbox_id,
        "daemon": DAEMON_NAME,
    })
}

pub(crate) fn validate_daemon_scope(request: &Request) -> Result<(), Value> {
    if request.scope.is_sandbox() {
        return Ok(());
    }
    Err(super::error_response(
        error_kind::INVALID_REQUEST,
        "daemon requests require sandbox scope",
        serde_json::json!({}),
    ))
}

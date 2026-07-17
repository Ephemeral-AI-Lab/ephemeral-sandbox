use std::sync::Arc;

use super::SandboxDaemonServer;
use crate::rpc::error::SandboxDaemonError;
use sandbox_observability_telemetry::record::names;
use sandbox_observability_telemetry::{SpanStatus, TraceContext};
use sandbox_operation_contract::{error, OperationRequest, OperationResponse};
use sandbox_protocol::{
    decode_request_value, error as wire_error, DAEMON_AUTH_FIELD, DAEMON_READINESS_OPERATION,
};
use serde_json::{Map, Value};

const DAEMON_NAME: &str = "sandbox-daemon";

impl SandboxDaemonServer {
    pub(crate) async fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> OperationResponse {
        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                return super::error_response(
                    wire_error::BAD_JSON,
                    format!("bad json: {err}"),
                    serde_json::json!({}),
                );
            }
        };
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
        match decode_request(value) {
            Ok(request) => self.dispatch_request(request).await,
            Err(response) => response,
        }
    }

    async fn dispatch_request(&self, request: OperationRequest) -> OperationResponse {
        if let Err(response) = validate_daemon_scope(&request) {
            return response;
        }
        if request.op == DAEMON_READINESS_OPERATION {
            return daemon_readiness_response(self.config.sandbox_id.as_deref(), &request);
        }
        if has_observability_handler(&request) {
            return self.dispatch_observability(request).await;
        }
        let operations = Arc::clone(&self.operations);
        let observer = self.observer();
        let task = tokio::task::spawn_blocking(move || {
            let ctx = TraceContext {
                trace: Arc::from(request.request_id.as_str()),
                parent: None,
            };
            observer.with_context(ctx, || {
                let dispatch = observer.span(names::DAEMON_DISPATCH);
                dispatch.attr("op", request.op.clone());
                let response = sandbox_runtime::dispatch_operation(&operations, &request);
                if response.as_json_value().get("error").is_some() {
                    dispatch.status(SpanStatus::Error);
                }
                response
            })
        });
        match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => super::error_response(
                error::INTERNAL_ERROR,
                "daemon request cancelled",
                serde_json::json!({}),
            ),
            Err(err) => super::error_response(
                error::INTERNAL_ERROR,
                format!("daemon request failed: {err}"),
                serde_json::json!({}),
            ),
        }
    }

    async fn dispatch_observability(&self, request: OperationRequest) -> OperationResponse {
        let operations = Arc::clone(&self.operations);
        let observability = self.observability.clone();
        let task = tokio::task::spawn_blocking(move || {
            let input = crate::observability::adapter::DaemonObservabilityAdapter::new(
                &operations,
                observability.as_deref(),
            );
            sandbox_observability_query::dispatch_operation(&input, &request)
        });
        match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => super::error_response(
                error::INTERNAL_ERROR,
                "daemon observability request cancelled",
                serde_json::json!({}),
            ),
            Err(err) => super::error_response(
                error::INTERNAL_ERROR,
                format!("daemon observability request failed: {err}"),
                serde_json::json!({}),
            ),
        }
    }
}

fn has_observability_handler(request: &OperationRequest) -> bool {
    sandbox_observability_query::observability_handler_keys().any(|(scope_kind, operation)| {
        scope_kind == request.scope.kind() && operation == request.op
    })
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

pub(crate) fn decode_request(value: Value) -> Result<OperationRequest, OperationResponse> {
    decode_request_value(value)
        .map_err(|err| super::error_response(err.kind(), err.message(), serde_json::json!({})))
}

/// Build the private readiness response. Proves the daemon decoded the request,
/// accepted the sandbox scope, and agrees with the expected sandbox id. When a
/// sandbox id is configured it must match the request's scope; otherwise the
/// request's scope id is echoed back.
pub(crate) fn daemon_readiness_response(
    configured_sandbox_id: Option<&str>,
    request: &OperationRequest,
) -> OperationResponse {
    let Some(requested) = request.scope.sandbox_id() else {
        return super::error_response(
            error::INVALID_REQUEST,
            format!("{DAEMON_READINESS_OPERATION} requires sandbox scope"),
            serde_json::json!({}),
        );
    };
    let sandbox_id = match configured_sandbox_id {
        Some(configured) if configured != requested => {
            return super::error_response(
                error::INVALID_REQUEST,
                format!(
                    "sandbox id mismatch: daemon is configured for {configured}, request targeted {requested}"
                ),
                serde_json::json!({}),
            );
        }
        Some(configured) => configured,
        None => requested,
    };
    OperationResponse::ok(serde_json::json!({
        "status": "ready",
        "sandbox_id": sandbox_id,
        "daemon": DAEMON_NAME,
    }))
}

pub(crate) fn validate_daemon_scope(request: &OperationRequest) -> Result<(), OperationResponse> {
    if request.scope.is_sandbox() {
        return Ok(());
    }
    Err(super::error_response(
        error::INVALID_REQUEST,
        "daemon requests require sandbox scope",
        serde_json::json!({}),
    ))
}

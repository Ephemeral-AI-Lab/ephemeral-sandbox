use std::sync::Arc;

use super::SandboxDaemonServer;
use crate::server::error::SandboxDaemonError;
use sandbox_protocol::{
    decode_request_value, error_kind, CliOperationScope, Request, DAEMON_AUTH_FIELD,
};
use serde_json::{Map, Value};
use tracing::{field, Instrument, Span};

impl SandboxDaemonServer {
    pub(crate) async fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> serde_json::Value {
        let span = tracing::info_span!(
            "daemon.request",
            sandbox_id = field::Empty,
            request_id = field::Empty,
            operation = field::Empty,
            scope_kind = field::Empty,
            transport = if is_tcp { "tcp" } else { "unix" },
            status = field::Empty,
            error_kind = field::Empty,
        );
        if let Some(sandbox_id) = self.config.sandbox_id.as_deref() {
            span.record("sandbox_id", sandbox_id);
        }
        let request_span = span.clone();
        async move {
            self.dispatch_bytes_in_span(bytes, is_tcp, &request_span)
                .await
        }
        .instrument(span)
        .await
    }

    async fn dispatch_bytes_in_span(
        &self,
        bytes: Vec<u8>,
        is_tcp: bool,
        span: &Span,
    ) -> serde_json::Value {
        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                record_error(span, error_kind::BAD_JSON);
                return super::error_response(
                    error_kind::BAD_JSON,
                    format!("bad json: {err}"),
                    serde_json::json!({}),
                );
            }
        };
        let value = if is_tcp {
            match self.strip_tcp_auth(value) {
                Ok(authenticated) => authenticated,
                Err(err) => {
                    record_error(span, err.response_kind());
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
            Ok(request) => self.dispatch_request(request, span).await,
            Err(response) => {
                record_response(span, &response);
                response
            }
        }
    }

    async fn dispatch_request(&self, request: Request, span: &Span) -> serde_json::Value {
        record_request(span, &request);
        if let Err(response) = validate_daemon_scope(&request) {
            record_response(span, &response);
            return response;
        }
        let operations = Arc::clone(&self.operations);
        let request_span = span.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _span_guard = request_span.enter();
            sandbox_runtime::dispatch_operation(&operations, &request).into_json_value()
        });
        let response = match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => {
                record_error(span, error_kind::INTERNAL_ERROR);
                super::error_response(
                    error_kind::INTERNAL_ERROR,
                    "daemon request cancelled",
                    serde_json::json!({}),
                )
            }
            Err(err) => {
                record_error(span, error_kind::INTERNAL_ERROR);
                super::error_response(
                    error_kind::INTERNAL_ERROR,
                    format!("daemon request failed: {err}"),
                    serde_json::json!({}),
                )
            }
        };
        record_response(span, &response);
        response
    }

    fn strip_tcp_auth(&self, mut value: serde_json::Value) -> Result<Value, SandboxDaemonError> {
        let expected_raw = configured_token(self.config.auth_token.as_deref());
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
}

fn record_request(span: &Span, request: &Request) {
    span.record("request_id", request.request_id.as_str());
    span.record("operation", operation_trace_label(&request.op));
    span.record("scope_kind", scope_kind(&request.scope));
}

fn operation_trace_label(operation: &str) -> &'static str {
    match operation {
        "exec_command" => "exec_command",
        "write_command_stdin" => "write_command_stdin",
        "read_command_lines" => "read_command_lines",
        _ => "unknown",
    }
}

fn scope_kind(scope: &CliOperationScope) -> &'static str {
    match scope {
        CliOperationScope::System => "system",
        CliOperationScope::Sandbox { .. } => "sandbox",
    }
}

fn record_response(span: &Span, response: &Value) {
    if let Some(kind) = response_error_kind(response) {
        record_error(span, kind);
    } else {
        span.record("status", "ok");
    }
}

fn record_error(span: &Span, kind: &str) {
    span.record("status", "error");
    span.record("error_kind", kind);
}

fn response_error_kind(response: &Value) -> Option<&str> {
    response
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str)
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

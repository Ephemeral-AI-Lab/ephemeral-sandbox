use std::sync::Arc;

use super::SandboxDaemonServer;
use crate::server::error::SandboxDaemonError;
use sandbox_protocol::{decode_request_value, error_kind, Request, DAEMON_AUTH_FIELD};
use sandbox_runtime::OperationTrace;
use serde_json::{Map, Value};

pub(crate) const PRIVATE_OBSERVABILITY_SNAPSHOT_OP: &str = "get_observability_snapshot";

impl SandboxDaemonServer {
    pub(crate) async fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> serde_json::Value {
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
        let value = if is_tcp {
            match self.strip_tcp_auth(value) {
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

    async fn dispatch_request(&self, request: Request) -> serde_json::Value {
        if let Err(response) = validate_daemon_scope(&request) {
            return response;
        }
        if request.op == PRIVATE_OBSERVABILITY_SNAPSHOT_OP {
            return self.dispatch_private_observability_snapshot(request).await;
        }
        let trace_sandbox_id = self
            .config
            .sandbox_id
            .as_ref()
            .filter(|sandbox_id| !sandbox_id.is_empty())
            .cloned();
        let observability = self.observability.clone();
        let trace =
            observability
                .as_ref()
                .zip(trace_sandbox_id.as_ref())
                .map(|(observability, _)| {
                    OperationTrace::new_with_enabled_span_keys(
                        observability.enabled_deep_span_keys(),
                    )
                });
        let trace_request_id = request.request_id.clone();
        let trace_operation = request.op.clone();
        let operations = Arc::clone(&self.operations);
        let task = tokio::task::spawn_blocking(move || {
            let response =
                sandbox_runtime::dispatch_operation(&operations, &request, trace.as_ref());
            let value = response.into_json_value();
            if let (Some(observability), Some(sandbox_id), Some(completed_trace)) = (
                observability,
                trace_sandbox_id,
                trace.as_ref().map(OperationTrace::complete),
            ) {
                let _ = observability.insert_completed_operation_trace(
                    sandbox_id,
                    trace_request_id,
                    trace_operation,
                    &value,
                    completed_trace,
                );
            }
            value
        });
        match task.await {
            Ok(response) => {
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

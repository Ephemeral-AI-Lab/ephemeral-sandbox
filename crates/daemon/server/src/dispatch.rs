use std::sync::Arc;

use super::DaemonServer;
use crate::error::DaemonError;
use daemon_operation::OperationRequest;
use sandbox_protocol::{decode_request_object, error_kind, ArgsPresence, DAEMON_AUTH_FIELD};
use serde_json::{Map, Value};

impl DaemonServer {
    pub(super) async fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> serde_json::Value {
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
        match parse_request(value) {
            Ok((op, request_id, args)) => self.dispatch_request(op, request_id, args).await,
            Err(response) => response,
        }
    }

    async fn dispatch_request(
        &self,
        op: String,
        request_id: String,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let op_for_error = op.clone();
        let operations = Arc::clone(&self.operations);
        let task = tokio::task::spawn_blocking(move || {
            daemon_operation::dispatch_operation(
                &operations,
                OperationRequest::new(&op, &request_id, &args),
            )
            .into_json_value()
        });
        let response = match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => super::error_response(
                error_kind::INTERNAL_ERROR,
                "daemon request cancelled",
                serde_json::json!({"op": op_for_error}),
            ),
            Err(err) => super::error_response(
                error_kind::INTERNAL_ERROR,
                format!("daemon request failed: {err}"),
                serde_json::json!({"op": op_for_error}),
            ),
        };
        response
    }

    fn strip_tcp_auth(&self, mut value: serde_json::Value) -> Result<Value, DaemonError> {
        let expected_raw = configured_token(self.config.auth_token.as_deref());
        if expected_raw.is_some() {
            let raw_token = match value.as_object_mut() {
                Some(object) => remove_token(object, DAEMON_AUTH_FIELD, expected_raw),
                None => TokenMatch::Missing,
            };
            if raw_token != TokenMatch::Matches {
                return Err(DaemonError::Unauthorized);
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

pub(crate) fn parse_request(value: Value) -> Result<(String, String, Value), Value> {
    let Value::Object(object) = value else {
        return Err(super::error_response(
            error_kind::BAD_JSON,
            "request message must be a json object",
            serde_json::json!({}),
        ));
    };
    let request = decode_request_object(object, ArgsPresence::Required)
        .map_err(|err| invalid_request(err.message()))?;
    Ok((request.op, request.request_id, request.args))
}

fn invalid_request(message: impl Into<String>) -> serde_json::Value {
    super::error_response(error_kind::INVALID_REQUEST, message, serde_json::json!({}))
}

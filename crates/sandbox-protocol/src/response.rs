use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::request::Request;

#[derive(Debug, Clone)]
pub struct SandboxResponse {
    value: Value,
}

pub type Response = SandboxResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Ok,
    Running,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseError {
    pub kind: String,
    pub message: String,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub duration_ms: Option<f64>,
    pub warnings: Vec<String>,
}

impl SandboxResponse {
    #[must_use]
    pub fn ok(_request: &Request<'_>, result: Value) -> Self {
        Self { value: result }
    }

    #[must_use]
    pub fn running(_request: &Request<'_>, result: Value) -> Self {
        Self { value: result }
    }

    #[must_use]
    pub fn service_error(_request: &Request<'_>, error: impl std::fmt::Display) -> Self {
        Self::fault("operation_failed", error.to_string())
    }

    #[must_use]
    pub fn unknown_op(request: &Request<'_>) -> Self {
        Self::fault("unknown_op", format!("unknown op: {}", request.name))
    }

    #[must_use]
    pub fn fault(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            value: json!({
                "error": {
                    "kind": kind,
                    "message": message.into(),
                    "details": {},
                },
            }),
        }
    }

    #[must_use]
    pub fn into_json_value(self) -> Value {
        self.value
    }
}

impl From<SandboxResponse> for Value {
    fn from(response: SandboxResponse) -> Self {
        response.into_json_value()
    }
}

#[must_use]
fn response_meta(op: &str, request_id: &str) -> Value {
    json!({
        "op": op,
        "request_id": request_id,
        "duration_ms": 0.0,
        "resource_summary": {"fields": {}},
        "warnings": [],
    })
}

#[must_use]
pub fn error_response_with_details(
    kind: &str,
    message: impl Into<String>,
    details: Value,
) -> Value {
    error_response_with_meta(kind, message, details, response_meta("", ""))
}

fn error_response_with_meta(
    kind: &str,
    message: impl Into<String>,
    details: Value,
    meta: Value,
) -> Value {
    json!({
        "status": "error",
        "error": {
            "kind": kind,
            "message": message.into(),
            "details": details,
        },
        "meta": meta,
    })
}

#[must_use]
pub fn response_line(response: &Value) -> Vec<u8> {
    crate::framing::encode_json_line(response)
}

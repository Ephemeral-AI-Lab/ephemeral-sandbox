use serde_json::{json, Value};

use crate::request::Request;

#[derive(Debug, Clone)]
pub struct Response {
    value: Value,
}

impl Response {
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

impl From<Response> for Value {
    fn from(response: Response) -> Self {
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

//! Request validation and unknown-operation responses after removal of the
//! legacy operation adapter.

use serde_json::{json, Value};

use crate::wire::{ErrorKind, Request};

use crate::response::error_envelope;

#[must_use]
pub fn dispatch(request: &Request) -> Value {
    if request.op.trim().is_empty() {
        return error_response(ErrorKind::InvalidRequest, "op is required", json!({}));
    }
    if !request.args.is_object() {
        return error_response(
            ErrorKind::InvalidRequest,
            "args must be an object",
            json!({}),
        );
    }
    error_response(
        ErrorKind::UnknownOp,
        format!("unknown op: {}", request.op),
        json!({"op": request.op}),
    )
}

#[must_use]
pub(crate) fn error_response(kind: ErrorKind, message: impl Into<String>, details: Value) -> Value {
    error_envelope(kind, message, details)
}

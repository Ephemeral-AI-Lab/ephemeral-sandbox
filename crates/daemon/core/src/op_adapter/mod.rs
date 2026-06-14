//! Daemon JSON operation adapters.
//!
//! These modules parse wire `args`, call the owning service/crate, and shape
//! the stable response object. Domain lifecycle policy should live below this
//! adapter layer.

pub(crate) mod checkpoint;
pub(crate) mod command;
pub(crate) mod control;
pub(crate) mod files;
pub(crate) mod isolation;
pub(crate) mod plugin;
pub(crate) mod workspace_run;

use operation::OpError;
use protocol::{FaultDetails, OperationEnvelope, OperationFault, ResponseMeta};
use serde::Serialize;
use serde_json::Value;

use crate::wire::ErrorKind;

pub(crate) fn to_wire_value(output: impl serde::Serialize) -> Value {
    serde_json::to_value(output).expect("operation output DTO serializes to JSON")
}

pub(crate) fn ok_envelope(output: impl Serialize) -> Value {
    let output = to_wire_value(output);
    if is_operation_envelope(&output) {
        return output;
    }
    to_wire_value(OperationEnvelope::ok(output, ResponseMeta::default()))
}

pub(crate) fn rejected_envelope(error: OpError) -> Value {
    to_wire_value(OperationEnvelope::<Value>::rejected(
        operation_fault(
            error.kind,
            error.message,
            error.details.unwrap_or_else(|| serde_json::json!({})),
        ),
        ResponseMeta::default(),
    ))
}

pub(crate) fn rejected_fault_envelope(
    kind: &'static str,
    message: impl Into<String>,
    details: Value,
) -> Value {
    rejected_envelope(OpError {
        kind,
        message: message.into(),
        details: Some(details),
    })
}

pub(crate) fn error_envelope(kind: ErrorKind, message: impl Into<String>, details: Value) -> Value {
    let fault = if kind == ErrorKind::InternalError {
        OperationFault::internal(message, fault_details(details))
    } else {
        operation_fault(error_kind_wire_name(kind), message, details)
    };
    to_wire_value(OperationEnvelope::<Value>::error(
        fault,
        ResponseMeta::default(),
    ))
}

pub(crate) fn is_operation_envelope(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some("ok" | "running" | "rejected" | "cancelled" | "timed_out" | "error") =
        object.get("status").and_then(Value::as_str)
    else {
        return false;
    };
    object.contains_key("meta") && (object.contains_key("result") || object.contains_key("error"))
}

fn operation_fault(
    kind: impl Into<String>,
    message: impl Into<String>,
    details: Value,
) -> OperationFault {
    OperationFault::new(kind, message).with_details(fault_details(details))
}

fn fault_details(details: Value) -> FaultDetails {
    match details {
        Value::Null => FaultDetails::default(),
        Value::Object(fields) if fields.is_empty() => FaultDetails::default(),
        Value::Object(fields) => fields
            .into_iter()
            .fold(FaultDetails::default(), |details, (key, value)| {
                details.with_field(key, value)
            }),
        value => FaultDetails::default().with_field("value", value),
    }
}

fn error_kind_wire_name(kind: ErrorKind) -> &'static str {
    kind.as_str()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{error_envelope, ok_envelope, rejected_fault_envelope};
    use crate::wire::ErrorKind;

    #[test]
    fn ok_envelope_wraps_result_without_flattening_domain_status() {
        let value = ok_envelope(json!({"success": true, "status": "committed"}));

        assert_eq!(value["status"], "ok");
        assert_eq!(value["result"]["status"], "committed");
        assert_eq!(value["result"]["success"], true);
        assert!(value.get("meta").is_some());
    }

    #[test]
    fn rejected_envelope_preserves_structured_detail_fields() {
        let value = rejected_fault_envelope(
            "invalid_argument",
            "caller_id is required",
            json!({"key": "caller_id"}),
        );

        assert_eq!(value["status"], "rejected");
        assert_eq!(value["error"]["kind"], "invalid_argument");
        assert_eq!(
            value["error"]["details"]["fields"],
            json!({"key": "caller_id"})
        );
    }

    #[test]
    fn internal_error_envelope_uses_explicit_error_id_and_detail_fields() {
        let value = error_envelope(
            ErrorKind::InternalError,
            "daemon invocation failed",
            json!({"op": "api.test.failure"}),
        );

        assert_eq!(value["status"], "error");
        assert_eq!(value["error"]["kind"], "internal_error");
        assert_eq!(
            value["error"]["details"]["fields"]["op"],
            "api.test.failure"
        );
        assert_eq!(value["error"]["error_id"].as_str().map(str::len), Some(32));
    }
}

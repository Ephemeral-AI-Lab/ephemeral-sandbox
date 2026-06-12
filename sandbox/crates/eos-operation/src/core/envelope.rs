use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use eos_trace::{RequestId, SpanUid, TraceId};

use super::fault::OperationFault;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Ok,
    Running,
    Rejected,
    Cancelled,
    TimedOut,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRef {
    pub trace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_span_id: Option<u64>,
    pub sidecar_event_count: u64,
    pub degraded: bool,
}

impl TraceRef {
    #[must_use]
    pub fn new(trace_id: &TraceId) -> Self {
        Self {
            trace_id: trace_id.to_string(),
            request_id: None,
            root_span_id: None,
            sidecar_event_count: 0,
            degraded: false,
        }
    }

    #[must_use]
    pub fn with_request(mut self, request_id: &RequestId) -> Self {
        self.request_id = Some(request_id.to_string());
        self
    }

    #[must_use]
    pub fn with_root_span(mut self, span_id: SpanUid) -> Self {
        self.root_span_id = Some(span_id.get());
        self
    }

    #[must_use]
    pub fn with_sidecar_event_count(mut self, count: u64) -> Self {
        self.sidecar_event_count = count;
        self
    }

    #[must_use]
    pub fn degraded(mut self) -> Self {
        self.degraded = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationWarning {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSummary {
    pub name: String,
    pub duration_us: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ResourceSummary {
    #[serde(default)]
    pub fields: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ResponseMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules_touched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<OperationWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_summary: Option<ResourceSummary>,
}

impl ResponseMeta {
    #[must_use]
    pub fn with_trace(mut self, trace: TraceRef) -> Self {
        self.trace = Some(trace);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationEnvelope<T> {
    pub status: OperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OperationFault>,
    #[serde(default)]
    pub meta: ResponseMeta,
}

impl<T> OperationEnvelope<T> {
    #[must_use]
    pub fn ok(result: T, meta: ResponseMeta) -> Self {
        Self {
            status: OperationStatus::Ok,
            result: Some(result),
            error: None,
            meta,
        }
    }

    #[must_use]
    pub fn running(result: T, meta: ResponseMeta) -> Self {
        Self {
            status: OperationStatus::Running,
            result: Some(result),
            error: None,
            meta,
        }
    }
}

impl<T> OperationEnvelope<T> {
    #[must_use]
    pub fn rejected(error: OperationFault, meta: ResponseMeta) -> Self {
        Self {
            status: OperationStatus::Rejected,
            result: None,
            error: Some(error),
            meta,
        }
    }

    #[must_use]
    pub fn cancelled(error: OperationFault, meta: ResponseMeta) -> Self {
        Self {
            status: OperationStatus::Cancelled,
            result: None,
            error: Some(error),
            meta,
        }
    }

    #[must_use]
    pub fn timed_out(error: OperationFault, meta: ResponseMeta) -> Self {
        Self {
            status: OperationStatus::TimedOut,
            result: None,
            error: Some(error),
            meta,
        }
    }

    #[must_use]
    pub fn error(error: OperationFault, meta: ResponseMeta) -> Self {
        Self {
            status: OperationStatus::Error,
            result: None,
            error: Some(error),
            meta,
        }
    }
}

/// Phase-01 migration bridge for old v1 flat responses. Phase 06 deletes this.
pub struct V1FlatteningAdapter;

impl V1FlatteningAdapter {
    #[must_use]
    pub fn from_legacy_value(value: Value) -> OperationEnvelope<Value> {
        if value.get("success").and_then(Value::as_bool) == Some(false) {
            let error = value.get("error").cloned().unwrap_or_else(|| json!({}));
            return OperationEnvelope::error(legacy_fault(error), ResponseMeta::default());
        }
        OperationEnvelope::ok(value, ResponseMeta::default())
    }

    pub fn to_legacy_value<T: Serialize>(envelope: &OperationEnvelope<T>) -> Value {
        match envelope.status {
            OperationStatus::Ok | OperationStatus::Running => {
                let mut value = envelope
                    .result
                    .as_ref()
                    .and_then(|result| serde_json::to_value(result).ok())
                    .unwrap_or_else(|| json!({}));
                if let Value::Object(object) = &mut value {
                    object
                        .entry("success".to_owned())
                        .or_insert(Value::Bool(true));
                }
                value
            }
            OperationStatus::Rejected
            | OperationStatus::Cancelled
            | OperationStatus::TimedOut
            | OperationStatus::Error => {
                let fault = envelope.error.as_ref().cloned().unwrap_or_else(|| {
                    OperationFault::new("internal_error", "operation failed without a fault")
                });
                json!({
                    "success": false,
                    "warnings": envelope.meta.warnings.iter().map(|warning| {
                        json!({"kind": warning.kind, "message": warning.message})
                    }).collect::<Vec<_>>(),
                    "timings": {},
                    "error": fault,
                })
            }
        }
    }
}

fn legacy_fault(error: Value) -> OperationFault {
    let kind = error
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("internal_error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("operation failed");
    let details = error.get("details").cloned().unwrap_or_else(|| json!({}));
    OperationFault::new(kind, message)
        .with_details(super::fault::FaultDetails::default().with_field("legacy_details", details))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FaultDetails;

    #[test]
    fn serializes_each_status_with_one_discriminant() {
        let meta = ResponseMeta::default();
        let envelopes = [
            OperationEnvelope::ok(json!({"ready": true}), meta.clone()),
            OperationEnvelope::running(json!({"command_id": "cmd-1"}), meta.clone()),
            OperationEnvelope::<Value>::rejected(
                OperationFault::new("forbidden", "blocked"),
                meta.clone(),
            ),
            OperationEnvelope::<Value>::cancelled(
                OperationFault::new("cancelled", "cancelled by caller"),
                meta.clone(),
            ),
            OperationEnvelope::<Value>::timed_out(
                OperationFault::new("timed_out", "deadline exceeded"),
                meta.clone(),
            ),
            OperationEnvelope::<Value>::error(
                OperationFault::internal("failed", FaultDetails::default()),
                meta,
            ),
        ];

        let statuses = envelopes.map(|envelope| {
            serde_json::to_value(envelope)
                .expect("envelope serializes")
                .get("status")
                .and_then(Value::as_str)
                .expect("status string")
                .to_owned()
        });

        assert_eq!(
            statuses,
            [
                "ok",
                "running",
                "rejected",
                "cancelled",
                "timed_out",
                "error"
            ]
        );
    }

    #[test]
    fn v1_adapter_is_confined_to_legacy_wire_shape() {
        let legacy = json!({
            "success": false,
            "error": {
                "kind": "bad_json",
                "message": "bad request",
                "details": {"line": 1}
            }
        });

        let envelope = V1FlatteningAdapter::from_legacy_value(legacy);
        assert_eq!(envelope.status, OperationStatus::Error);
        assert_eq!(envelope.error.expect("fault").kind, "bad_json");

        let legacy_again = V1FlatteningAdapter::to_legacy_value(&OperationEnvelope::ok(
            json!({"ready": true}),
            ResponseMeta::default(),
        ));
        assert_eq!(legacy_again["success"], true);
        assert_eq!(legacy_again["ready"], true);
    }
}

use serde_json::{json, Value};

use super::ForwardAttempt;

pub(super) fn mark_response_trace_ingested(attempt: &ForwardAttempt<'_>, response: &mut Value) {
    let Some(object) = response.as_object_mut() else {
        return;
    };
    if object.get("status").and_then(Value::as_str).is_none() {
        return;
    }
    let Some(meta) = object.get_mut("meta").and_then(Value::as_object_mut) else {
        return;
    };
    meta.insert(
        "request_id".to_owned(),
        Value::String(attempt.request_id.to_string()),
    );
    let trace = meta
        .entry("trace".to_owned())
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(trace) = trace else {
        return;
    };
    trace.insert(
        "trace_id".to_owned(),
        Value::String(attempt.trace_id.to_string()),
    );
    trace.insert(
        "request_id".to_owned(),
        Value::String(attempt.request_id.to_string()),
    );
    trace.insert("store".to_owned(), Value::String("local_sqlite".to_owned()));
    trace.insert("degraded".to_owned(), Value::Bool(false));
}

pub(super) fn refresh_response_trace_receipt(attempt: &ForwardAttempt<'_>, response: &mut Value) {
    let Some(object) = response.as_object_mut() else {
        return;
    };
    let Some(meta) = object.get_mut("meta").and_then(Value::as_object_mut) else {
        return;
    };
    let trace = meta
        .entry("trace".to_owned())
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(trace) = trace else {
        return;
    };
    if let Ok(count) = attempt
        .trace_store
        .event_count_for_trace(attempt.trace_id.as_str())
    {
        trace.insert("event_count".to_owned(), json!(count));
    }
    trace.insert(
        "trace_id".to_owned(),
        Value::String(attempt.trace_id.to_string()),
    );
    trace.insert(
        "request_id".to_owned(),
        Value::String(attempt.request_id.to_string()),
    );
    trace.insert("store".to_owned(), Value::String("local_sqlite".to_owned()));
}

pub(super) fn mark_response_trace_degraded(
    attempt: &ForwardAttempt<'_>,
    response: &mut Value,
    error_kind: &str,
    message: &str,
) {
    let Some(object) = response.as_object_mut() else {
        return;
    };
    if object.get("status").and_then(Value::as_str).is_none() {
        return;
    }
    let Some(meta) = object.get_mut("meta").and_then(Value::as_object_mut) else {
        return;
    };
    meta.insert(
        "request_id".to_owned(),
        Value::String(attempt.request_id.to_string()),
    );
    let trace = meta
        .entry("trace".to_owned())
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(trace) = trace else {
        return;
    };
    trace.insert(
        "trace_id".to_owned(),
        Value::String(attempt.trace_id.to_string()),
    );
    trace.insert(
        "request_id".to_owned(),
        Value::String(attempt.request_id.to_string()),
    );
    trace.insert("store".to_owned(), Value::String("local_sqlite".to_owned()));
    trace.insert("degraded".to_owned(), Value::Bool(true));
    trace.insert(
        "degraded_reason".to_owned(),
        json!({"kind": error_kind, "message": message}),
    );
}

use serde_json::Value;
use trace::{SpanKind, SpanRecord, SpanStatus, SpanUid};

use super::resources::optional_u64;
use super::RequestTraceEvent;

pub(super) const COMMAND_PROCESS_SPAWN_SPAN_ID: SpanUid = SpanUid::new(5);
pub(super) const COMMAND_PROCESS_WAIT_SPAN_ID: SpanUid = SpanUid::new(6);

pub(super) fn child_spans_from_request_events(
    events: &[RequestTraceEvent],
    now: u64,
) -> Vec<SpanRecord> {
    let mut spans = Vec::new();
    if let Some(event) = events
        .iter()
        .find(|event| event.module == "command" && event.name == "spawned")
    {
        spans.push(command_process_span(
            COMMAND_PROCESS_SPAWN_SPAN_ID,
            "command.process.spawn",
            SpanKind::CommandProcessSpawn,
            &event.details,
            now,
        ));
    }
    if let Some(event) = events
        .iter()
        .find(|event| event.module == "command" && event.name == "wait_finished")
    {
        spans.push(command_process_span(
            COMMAND_PROCESS_WAIT_SPAN_ID,
            "command.process.wait",
            SpanKind::CommandProcessWait,
            &event.details,
            now,
        ));
    }
    spans
}

fn command_process_span(
    span_id: SpanUid,
    name: &'static str,
    kind: SpanKind,
    details: &Value,
    now: u64,
) -> SpanRecord {
    let duration_us = optional_u64(details.get("duration_us"))
        .or_else(|| optional_u64(details.get("duration_ms")).map(|ms| ms.saturating_mul(1_000)))
        .unwrap_or(0);
    let mut span = SpanRecord::new(span_id, Some(SpanUid::new(4)), name, kind, details.clone());
    span.started_at_unix_ms = now.saturating_sub(duration_us / 1_000);
    span.finished_at_unix_ms = now;
    span.duration_us = duration_us;
    span.status = command_span_status_from_details(details);
    span
}

fn command_span_status_from_details(details: &Value) -> Option<SpanStatus> {
    if details.get("success").and_then(Value::as_bool) == Some(false) {
        return Some(SpanStatus::Error);
    }
    let status = details.get("status").and_then(Value::as_str)?;
    if status == "running" {
        Some(SpanStatus::Ok)
    } else {
        SpanStatus::parse_label(status)
    }
}

pub(super) fn request_event_span_id(event: &RequestTraceEvent) -> SpanUid {
    if event.module == "command" {
        return match event.name.as_str() {
            "spawned" => COMMAND_PROCESS_SPAWN_SPAN_ID,
            "artifact_written"
                if event.details.get("artifact").and_then(Value::as_str)
                    == Some("runner_request") =>
            {
                COMMAND_PROCESS_SPAWN_SPAN_ID
            }
            "wait_finished" | "yielded" | "response_meta" => COMMAND_PROCESS_WAIT_SPAN_ID,
            _ => event.span_id,
        };
    }
    if event.module == "resource"
        && event
            .details
            .get("meta")
            .and_then(|meta| meta.get("source"))
            .and_then(Value::as_str)
            == Some("command.process.wait")
    {
        return COMMAND_PROCESS_WAIT_SPAN_ID;
    }
    event.span_id
}

pub(super) fn op_family(op: &str) -> &str {
    op.split('.').nth(1).unwrap_or("unknown")
}

pub(super) fn op_verb(op: &str) -> &str {
    op.rsplit('.').next().unwrap_or("unknown")
}

pub(super) fn op_span_name(op: &str) -> String {
    format!("op.{}.{}", op_family(op), op_verb(op))
}

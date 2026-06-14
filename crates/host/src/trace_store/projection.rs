use rusqlite::{params, Transaction};
use serde_json::{json, Value};

use super::payload::{HostTraceEventPayload, ResponsePersistedPayload, TraceDegradedPayload};
use super::u64_to_i64;

pub(super) struct ProjectRequestStart<'a> {
    pub(super) sandbox_id: &'a str,
    pub(super) trace_id: &'a str,
    pub(super) request_id: &'a str,
    pub(super) op: &'a str,
    pub(super) family: &'a str,
    pub(super) caller_id: Option<&'a str>,
    pub(super) args_summary: &'a str,
    pub(super) args_digest: &'a str,
    pub(super) sent_at_ms: u64,
    pub(super) host_boot_id: &'a str,
}

pub(super) fn project_request_start_tx(
    tx: &Transaction<'_>,
    row: ProjectRequestStart<'_>,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "INSERT OR REPLACE INTO trace_requests
         (request_id, trace_id, sandbox_id, op, family, caller_id, args_summary,
          args_digest, sent_at_ms, host_boot_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.request_id,
            row.trace_id,
            row.sandbox_id,
            row.op,
            row.family,
            row.caller_id,
            row.args_summary,
            row.args_digest,
            row.sent_at_ms,
            row.host_boot_id,
        ],
    )?;
    Ok(())
}

pub(super) fn project_trace_degraded_tx(
    tx: &Transaction<'_>,
    payload: &TraceDegradedPayload,
) -> Result<(), rusqlite::Error> {
    project_request_start_tx(
        tx,
        ProjectRequestStart {
            sandbox_id: &payload.sandbox_id,
            trace_id: &payload.trace_id,
            request_id: &payload.request_id,
            op: &payload.op,
            family: &payload.family,
            caller_id: payload.caller_id.as_deref(),
            args_summary: &payload.args_summary,
            args_digest: &payload.args_digest,
            sent_at_ms: payload.sent_at_ms,
            host_boot_id: &payload.host_boot_id,
        },
    )?;
    tx.execute(
        "UPDATE trace_requests
         SET status='trace_degraded', error_kind=?2, response_summary=?3
         WHERE request_id=?1",
        params![payload.request_id, payload.error_kind, payload.message],
    )?;
    Ok(())
}

pub(super) fn project_trace_batch_tx(
    tx: &Transaction<'_>,
    batch: &trace::TraceBatch,
) -> Result<(), rusqlite::Error> {
    let daemon_boot_id = batch
        .daemon_boot_id
        .as_deref()
        .filter(|boot_id| !boot_id.is_empty());
    for record in &batch.records {
        let trace_id = record.trace_id.to_string();
        let request_id = record.request_id.as_ref().map(ToString::to_string);
        for span in &record.spans {
            tx.execute(
                "INSERT OR REPLACE INTO trace_spans
                 (trace_id, request_id, span_id, parent_span_id, kind, subsystem, status,
                  started_us, duration_us, fields_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    trace_id,
                    request_id,
                    u64_to_i64(span.span_id.get()),
                    span.parent_span_id.map(|id| u64_to_i64(id.get())),
                    serde_label(span.kind),
                    serde_label(span.subsystem),
                    span.status.map_or_else(|| "ok".to_owned(), serde_label),
                    u64_to_i64(span.started_at_unix_ms.saturating_mul(1000)),
                    u64_to_i64(span.duration_us),
                    span.fields.encoded_value(),
                ],
            )?;
        }
        for event in &record.events {
            let seq = next_trace_seq(tx, &trace_id)?;
            tx.execute(
                "INSERT INTO trace_events
                 (trace_id, seq, request_id, span_id, module, event, level, ts_us, details_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'info', ?7, ?8)",
                params![
                    trace_id,
                    seq,
                    request_id,
                    u64_to_i64(event.span_id.get()),
                    event.module,
                    event.name,
                    u64_to_i64(event.at_unix_ms.saturating_mul(1000)),
                    event.details.encoded_value(),
                ],
            )?;
        }
        for resource in &record.resources {
            let values = json!({
                "phase": resource.meta.phase,
                "source": resource.meta.source,
                "source_available": resource.meta.source_available,
                "read_error": resource.meta.read_error,
                "parse_error": resource.meta.parse_error,
                "sampler_duration_us": resource.meta.sampler_duration_us,
                "inflight_requests": resource.meta.inflight_requests,
                "payload": resource.payload.value,
            });
            tx.execute(
                "INSERT INTO trace_resources
                 (trace_id, request_id, span_id, ts_us, kind, values_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    trace_id,
                    request_id,
                    resource.span_id.map(|span_id| u64_to_i64(span_id.get())),
                    u64_to_i64(record.finished_at_unix_ms.saturating_mul(1000)),
                    resource.meta.stats_kind.as_str(),
                    values.to_string(),
                ],
            )?;
        }
        for link in &record.links {
            let link_request_id = request_id.as_deref().unwrap_or_default();
            tx.execute(
                "INSERT OR IGNORE INTO trace_links
                 (trace_id, link_kind, link_id, request_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    trace_id,
                    serde_label(link.kind),
                    link.value,
                    link_request_id
                ],
            )?;
        }
        if let Some(request_id) = &request_id {
            project_request_rollup_tx(tx, request_id, record, daemon_boot_id)?;
        }
    }
    Ok(())
}

/// Denormalized `trace_requests` rollups derived from the daemon batch: root
/// span duration, recorded workspace route, daemon boot id, and the ordered
/// distinct span subsystems.
fn project_request_rollup_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    record: &trace::TraceRecord,
    daemon_boot_id: Option<&str>,
) -> Result<(), rusqlite::Error> {
    let duration_us = record
        .spans
        .iter()
        .find(|span| span.span_id == record.root_span_id)
        .map(|span| u64_to_i64(span.duration_us));
    let workspace_route = record
        .events
        .iter()
        .filter(|event| event.module == "workspace.route" && event.name == "route_selected")
        .find_map(|event| event.details.value.get("kind").and_then(Value::as_str))
        .filter(|kind| {
            matches!(
                *kind,
                "ephemeral_workspace" | "isolated_workspace" | "fast_path" | "none"
            )
        });
    let mut modules: Vec<String> = Vec::new();
    for span in &record.spans {
        let subsystem = serde_label(span.subsystem);
        if !modules.contains(&subsystem) {
            modules.push(subsystem);
        }
    }
    let modules_touched =
        (!modules.is_empty()).then(|| serde_json::to_string(&modules).unwrap_or_default());
    tx.execute(
        "UPDATE trace_requests
         SET workspace_route=COALESCE(?2, workspace_route),
             duration_us=COALESCE(?3, duration_us),
             daemon_boot_id=COALESCE(?4, daemon_boot_id),
             modules_touched=COALESCE(?5, modules_touched)
         WHERE request_id=?1",
        params![
            request_id,
            workspace_route,
            duration_us,
            daemon_boot_id,
            modules_touched
        ],
    )?;
    Ok(())
}

pub(super) fn project_host_trace_event_tx(
    tx: &Transaction<'_>,
    payload: &HostTraceEventPayload,
) -> Result<(), rusqlite::Error> {
    let seq = next_trace_seq(tx, &payload.trace_id)?;
    tx.execute(
        "INSERT INTO trace_events
         (trace_id, seq, request_id, span_id, module, event, level, ts_us, details_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'info', ?7, ?8)",
        params![
            payload.trace_id,
            seq,
            payload.request_id,
            payload.span_id,
            payload.module,
            payload.event,
            u64_to_i64(payload.ts_us),
            payload.details_json,
        ],
    )?;
    Ok(())
}

pub(super) fn project_response_persisted_tx(
    tx: &Transaction<'_>,
    payload: &ResponsePersistedPayload,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "UPDATE trace_requests
         SET status=?2, error_kind=?3, received_at_ms=?4, host_rtt_ms=?5,
             response_digest=?6, response_len=?7, response_summary=?8
         WHERE request_id=?1",
        params![
            payload.request_id,
            payload.status,
            payload.error_kind,
            payload.received_at_ms,
            payload.host_rtt_ms,
            payload.response_digest,
            payload.response_len,
            payload.response_summary,
        ],
    )?;
    Ok(())
}

fn next_trace_seq(tx: &Transaction<'_>, trace_id: &str) -> Result<i64, rusqlite::Error> {
    tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM trace_events WHERE trace_id=?1",
        params![trace_id],
        |row| row.get(0),
    )
}

fn serde_label<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

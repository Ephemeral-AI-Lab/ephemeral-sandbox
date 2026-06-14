use protocol::{
    ResourceSummary, ResponseMeta, StepSummary, TraceRef, WorkspaceRouteRef, ENVELOPE_VERSION,
};
use serde_json::{json, Value};
use trace::{SpanStatus, SpanSubsystem, TraceRecord, WorkspaceRoute};

pub(super) fn stamp_pending_envelope_meta(
    object: &mut serde_json::Map<String, Value>,
    record: &TraceRecord,
    op: &str,
    duration_us: u64,
) {
    if object.get("status").and_then(Value::as_str).is_none() {
        return;
    }
    let request_id = record
        .request_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let trace_ref = TraceRef {
        trace_id: record.trace_id.to_string(),
        request_id: (!request_id.is_empty()).then_some(request_id.clone()),
        root_span_id: Some(record.root_span_id.get()),
        store: "pending_host_ingest".to_owned(),
        event_count: u64::try_from(record.events.len()).unwrap_or(u64::MAX),
        degraded: record.truncated,
    };
    let meta = ResponseMeta {
        envelope_version: ENVELOPE_VERSION,
        op: op.to_owned(),
        request_id,
        trace: trace_ref,
        caller_id: None,
        workspace_route: workspace_route_ref(record),
        duration_ms: duration_us as f64 / 1000.0,
        modules_touched: modules_touched(record),
        steps: step_summaries(record),
        resource_summary: resource_summary(record),
        warnings: Vec::new(),
    };
    object.insert(
        "meta".to_owned(),
        serde_json::to_value(meta).expect("response meta serializes"),
    );
}

/// Bounded rollup of the record's resource samples, rendered from the trace
/// record (never hand-inserted). Empty when no resources were sampled so
/// control/no-resource ops keep an empty summary.
fn resource_summary(record: &TraceRecord) -> ResourceSummary {
    if record.resources.is_empty() {
        return ResourceSummary::default();
    }
    let mut kinds: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    let mut unavailable = 0u64;
    let mut source_errors = 0u64;
    for resource in &record.resources {
        *kinds.entry(resource.meta.stats_kind.as_str()).or_default() += 1;
        if !resource.meta.source_available {
            unavailable += 1;
        }
        if resource.meta.read_error.is_some() || resource.meta.parse_error.is_some() {
            source_errors += 1;
        }
    }
    let mut fields = serde_json::Map::new();
    fields.insert("samples".to_owned(), json!(record.resources.len()));
    fields.insert("kinds".to_owned(), json!(kinds));
    if unavailable > 0 {
        fields.insert("unavailable".to_owned(), json!(unavailable));
    }
    if source_errors > 0 {
        fields.insert("source_errors".to_owned(), json!(source_errors));
    }
    ResourceSummary { fields }
}

fn workspace_route_ref(record: &TraceRecord) -> WorkspaceRouteRef {
    record
        .events
        .iter()
        .find(|event| event.module == "workspace.route" && event.name == "route_selected")
        .map_or_else(WorkspaceRouteRef::default, |event| {
            let details = &event.details.value;
            WorkspaceRouteRef {
                kind: details
                    .get("kind")
                    .and_then(Value::as_str)
                    .and_then(parse_workspace_route)
                    .unwrap_or(WorkspaceRoute::None),
                reason: details
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
}

fn parse_workspace_route(label: &str) -> Option<WorkspaceRoute> {
    Some(match label {
        "ephemeral_workspace" => WorkspaceRoute::EphemeralWorkspace,
        "isolated_workspace" => WorkspaceRoute::IsolatedWorkspace,
        "fast_path" => WorkspaceRoute::FastPath,
        "none" => WorkspaceRoute::None,
        _ => return None,
    })
}

fn modules_touched(record: &TraceRecord) -> Vec<SpanSubsystem> {
    let mut modules = Vec::new();
    for span in &record.spans {
        if !modules.contains(&span.subsystem) {
            modules.push(span.subsystem);
        }
    }
    modules
}

fn step_summaries(record: &TraceRecord) -> Vec<StepSummary> {
    record
        .spans
        .iter()
        .filter(|span| span.parent_span_id == Some(record.root_span_id))
        .map(|span| StepSummary {
            kind: span.name.clone(),
            duration_us: span.duration_us,
            status: span.status.unwrap_or(SpanStatus::Ok),
        })
        .collect()
}

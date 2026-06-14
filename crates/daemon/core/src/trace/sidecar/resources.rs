use serde_json::Value;
use trace::{BoundedJson, DetailBudget, ResourceStats, ResourceStatsKind, ResourceStatsMeta};

use super::events::request_event_span_id;
use super::RequestTraceEvent;

pub(super) fn resource_stats_from_event(event: &RequestTraceEvent) -> Option<ResourceStats> {
    if event.module != "resource" || event.name != "resource_stats" {
        return None;
    }
    let details = event.details.as_object()?;
    let meta = details.get("meta")?.as_object()?;
    let stats_kind = resource_stats_kind(meta.get("stats_kind")?.as_str()?);
    let source = meta.get("source")?.as_str()?.to_owned();
    let source_available = meta
        .get("source_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let phase = meta.get("phase").and_then(Value::as_str).map(str::to_owned);
    let read_error = optional_string(meta.get("read_error"));
    let parse_error = optional_string(meta.get("parse_error"));
    let sampler_duration_us = optional_u64(meta.get("sampler_duration_us")).unwrap_or(0);
    let inflight_requests = optional_u64(meta.get("inflight_requests")).unwrap_or(0);
    let mut payload = event.details.clone();
    if let Some(payload_object) = payload.as_object_mut() {
        payload_object.remove("meta");
    }
    Some(ResourceStats {
        span_id: Some(request_event_span_id(event)),
        meta: ResourceStatsMeta {
            stats_kind,
            phase,
            source,
            source_available,
            read_error,
            parse_error,
            sampler_duration_us,
            inflight_requests,
        },
        payload: BoundedJson::capture(payload, DetailBudget::EventDetails),
    })
}

fn resource_stats_kind(label: &str) -> ResourceStatsKind {
    match label {
        "tree" => ResourceStatsKind::Tree,
        "host" => ResourceStatsKind::Host,
        "mount_cost" => ResourceStatsKind::MountCost,
        _ => ResourceStatsKind::CgroupProcess,
    }
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

pub(super) fn optional_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).or_else(|| {
        value
            .and_then(Value::as_i64)
            .and_then(|value| value.try_into().ok())
    })
}

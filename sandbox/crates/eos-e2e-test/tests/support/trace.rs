#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use eos_e2e_test::client::{
    decode_trace_sidecar_base64, response_status, take_trace_sidecar_checked,
    DAEMON_TRACE_SIDECAR_FIELD,
};
use eos_operation::{OperationEnvelope, ResponseMeta, TraceRef};
use eos_sandbox_host::trace_store::{TraceRequestRow, TraceStore};
use eos_trace::{decode_trace_batch, TraceRecord};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResponseTraceIds {
    pub trace_id: String,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredTraceSummary {
    pub request: TraceRequestRow,
    pub span_count: usize,
    pub event_count: usize,
    pub resource_count: usize,
}

pub(crate) fn operation_envelope(response: &Value) -> Result<OperationEnvelope<Value>> {
    serde_json::from_value(response.clone())
        .with_context(|| format!("decode OperationEnvelope from response: {response}"))
}

pub(crate) fn envelope_meta(response: &Value) -> Result<ResponseMeta> {
    serde_json::from_value(
        response
            .get("meta")
            .cloned()
            .with_context(|| format!("response missing envelope meta: {response}"))?,
    )
    .with_context(|| format!("decode ResponseMeta from response: {response}"))
}

pub(crate) fn envelope_result(response: &Value) -> Result<&Value> {
    response
        .get("result")
        .with_context(|| format!("response missing envelope result: {response}"))
}

pub(crate) fn envelope_status(response: &Value) -> Result<&str> {
    response
        .get("status")
        .and_then(Value::as_str)
        .with_context(|| format!("response missing envelope status: {response}"))
}

pub(crate) fn envelope_error_kind(response: &Value) -> Result<&str> {
    response
        .get("error")
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str)
        .with_context(|| format!("response missing envelope error kind: {response}"))
}

pub(crate) fn envelope_error_kind_or_status(response: &Value) -> Result<String> {
    if response.get("error").is_some() {
        Ok(envelope_error_kind(response)?.to_owned())
    } else {
        Ok(envelope_status(response)?.to_owned())
    }
}

pub(crate) fn response_trace_ids(response: &Value) -> Result<ResponseTraceIds> {
    let meta = envelope_meta(response)?;
    assert_consistent_trace_ref(&meta.trace, &meta.request_id)?;
    Ok(ResponseTraceIds {
        trace_id: meta.trace.trace_id,
        request_id: meta.request_id,
    })
}

pub(crate) fn assert_no_trace_sidecar(response: &Value) -> Result<()> {
    if response.get(DAEMON_TRACE_SIDECAR_FIELD).is_some() {
        bail!("gateway-facing response exposed {DAEMON_TRACE_SIDECAR_FIELD}: {response}");
    }
    Ok(())
}

pub(crate) fn assert_response_trace_ref_in_store(
    response: &Value,
    store: &TraceStore,
) -> Result<StoredTraceSummary> {
    assert_no_trace_sidecar(response)?;
    let meta = envelope_meta(response)?;
    assert_consistent_trace_ref(&meta.trace, &meta.request_id)?;
    if meta.trace.store != "local_sqlite" {
        bail!("response trace ref was not host-ingested: {:?}", meta.trace);
    }

    let request = store
        .request_by_id(&meta.request_id)
        .with_context(|| format!("query trace request {}", meta.request_id))?
        .with_context(|| format!("trace request {} missing from host store", meta.request_id))?;
    if request.trace_id != meta.trace.trace_id {
        bail!(
            "store trace_id {} did not match response trace_id {} for request {}",
            request.trace_id,
            meta.trace.trace_id,
            meta.request_id
        );
    }

    let status = response_status(response);
    if request.status.as_deref() != Some(status) {
        bail!(
            "store status {:?} did not match response status {status} for request {}",
            request.status,
            meta.request_id
        );
    }

    let span_count = store
        .span_count_for_request(&meta.trace.trace_id, &meta.request_id)
        .with_context(|| format!("count stored spans for request {}", meta.request_id))?;
    let event_count = store
        .event_count_for_trace(&meta.trace.trace_id)
        .with_context(|| format!("count stored events for trace {}", meta.trace.trace_id))?;
    let resource_count = store
        .resource_count_for_request(&meta.request_id)
        .with_context(|| format!("count stored resources for request {}", meta.request_id))?;

    let ref_event_count = usize::try_from(meta.trace.event_count).unwrap_or(usize::MAX);
    if event_count < ref_event_count {
        bail!(
            "store has {event_count} events but response trace ref reported at least {ref_event_count}"
        );
    }
    if span_count == 0 && event_count == 0 && resource_count == 0 {
        bail!(
            "host store has no spans, events, or resources for request {}",
            meta.request_id
        );
    }

    Ok(StoredTraceSummary {
        request,
        span_count,
        event_count,
        resource_count,
    })
}

pub(crate) fn trace_record(response: &Value) -> Result<TraceRecord> {
    let mut stripped = response.clone();
    let sidecar = take_trace_sidecar_checked(&mut stripped)
        .map_err(|err| anyhow::anyhow!("malformed trace sidecar {}: {response}", err.kind()))?
        .with_context(|| format!("response missing trace sidecar: {response}"))?;
    let batch = decode_trace_batch(&sidecar).context("decode trace sidecar")?;
    let mut records = batch.records;
    if records.len() != 1 {
        bail!(
            "expected one trace record in response sidecar, got {}",
            records.len()
        );
    }
    Ok(records.remove(0))
}

pub(crate) fn trace_export_records(response: &Value) -> Result<Vec<TraceRecord>> {
    let Some(encoded) = response.get("trace_batch_base64").and_then(Value::as_str) else {
        if response.get("record_count").and_then(Value::as_i64) == Some(0) {
            return Ok(Vec::new());
        }
        bail!("trace export missing trace_batch_base64: {response}");
    };
    let bytes = decode_trace_sidecar_base64(encoded).context("decode trace export batch")?;
    Ok(decode_trace_batch(&bytes)
        .context("decode trace export protobuf")?
        .records)
}

pub(crate) fn has_trace_event(
    record: &TraceRecord,
    module: &str,
    name: &str,
    predicate: impl Fn(&Value) -> bool,
) -> bool {
    record.events.iter().any(|event| {
        event.module == module && event.name == name && predicate(&event.details.value)
    })
}

fn assert_consistent_trace_ref(trace: &TraceRef, request_id: &str) -> Result<()> {
    if trace.trace_id.is_empty() {
        bail!("response trace ref did not include trace_id");
    }
    if request_id.is_empty() {
        bail!("response meta did not include request_id");
    }
    if let Some(trace_request_id) = trace.request_id.as_deref() {
        if trace_request_id != request_id {
            bail!(
                "response meta.request_id {request_id} did not match trace.request_id {trace_request_id}"
            );
        }
    }
    Ok(())
}

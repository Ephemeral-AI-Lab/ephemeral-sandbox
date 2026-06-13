use prost::Message;

use crate::budget::BoundedJson;
use crate::ids::{RequestId, SpanUid, TraceId};
use crate::record::{
    EventRecord, SpanKind, SpanRecord, SpanStatus, SpanSubsystem, TraceKind, TraceLink,
    TraceLinkKind, TraceRecord,
};
use crate::resource_stats::{ResourceStats, ResourceStatsKind, ResourceStatsMeta};

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/eos.trace.v1.rs"));
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceBatch {
    pub records: Vec<TraceRecord>,
    pub dropped_traces: u64,
    pub daemon_boot_id: Option<String>,
}

impl TraceBatch {
    #[must_use]
    pub fn single(record: TraceRecord) -> Self {
        Self {
            records: vec![record],
            dropped_traces: 0,
            daemon_boot_id: None,
        }
    }
}

#[derive(Debug)]
pub enum DecodeTraceError {
    Protobuf(prost::DecodeError),
    Invalid(String),
}

impl std::fmt::Display for DecodeTraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protobuf(err) => write!(f, "failed to decode trace protobuf: {err}"),
            Self::Invalid(message) => write!(f, "invalid trace protobuf: {message}"),
        }
    }
}

impl std::error::Error for DecodeTraceError {}

#[must_use]
pub fn encode_trace_batch(batch: &TraceBatch) -> Vec<u8> {
    let proto = proto::TraceBatch {
        records: batch.records.iter().map(record_to_proto).collect(),
        dropped_traces: batch.dropped_traces,
        daemon_boot_id: batch.daemon_boot_id.clone().unwrap_or_default(),
    };
    proto.encode_to_vec()
}

pub fn decode_trace_batch(bytes: &[u8]) -> Result<TraceBatch, DecodeTraceError> {
    proto::TraceBatch::decode(bytes)
        .map_err(DecodeTraceError::Protobuf)
        .and_then(proto_to_batch)
}

#[must_use]
pub fn encoded_trace_record_len(record: &TraceRecord) -> usize {
    encode_trace_batch(&TraceBatch::single(record.clone())).len()
}

fn proto_to_batch(batch: proto::TraceBatch) -> Result<TraceBatch, DecodeTraceError> {
    Ok(TraceBatch {
        records: batch
            .records
            .into_iter()
            .enumerate()
            .map(|(index, record)| proto_to_record(record, index))
            .collect::<Result<Vec<_>, _>>()?,
        dropped_traces: batch.dropped_traces,
        daemon_boot_id: (!batch.daemon_boot_id.is_empty()).then_some(batch.daemon_boot_id),
    })
}

fn record_to_proto(record: &TraceRecord) -> proto::TraceRecord {
    proto::TraceRecord {
        trace_id: record.trace_id.to_string(),
        request_id: record
            .request_id
            .as_ref()
            .map_or_else(String::new, ToString::to_string),
        kind: trace_kind_code(record.kind),
        root_span_id: record.root_span_id.get(),
        started_at_unix_ms: record.started_at_unix_ms,
        finished_at_unix_ms: record.finished_at_unix_ms,
        spans: record.spans.iter().map(span_to_proto).collect(),
        events: record.events.iter().map(event_to_proto).collect(),
        links: record.links.iter().map(link_to_proto).collect(),
        resources: record.resources.iter().map(resource_to_proto).collect(),
        dropped_children: record.dropped_children,
        truncated: record.truncated,
    }
}

fn proto_to_record(
    record: proto::TraceRecord,
    record_index: usize,
) -> Result<TraceRecord, DecodeTraceError> {
    let trace_id = TraceId::parse(record.trace_id)
        .map_err(|err| invalid(format!("records[{record_index}].trace_id: {err}")))?;
    let request_id = if record.request_id.is_empty() {
        None
    } else {
        Some(
            RequestId::parse(record.request_id)
                .map_err(|err| invalid(format!("records[{record_index}].request_id: {err}")))?,
        )
    };
    Ok(TraceRecord {
        trace_id,
        request_id,
        kind: trace_kind_from_code(record.kind, record_index)?,
        root_span_id: SpanUid::new(record.root_span_id),
        started_at_unix_ms: record.started_at_unix_ms,
        finished_at_unix_ms: record.finished_at_unix_ms,
        spans: record
            .spans
            .into_iter()
            .enumerate()
            .map(|(index, span)| proto_to_span(span, record_index, index))
            .collect::<Result<Vec<_>, _>>()?,
        events: record
            .events
            .into_iter()
            .enumerate()
            .map(|(index, event)| proto_to_event(event, record_index, index))
            .collect::<Result<Vec<_>, _>>()?,
        links: record
            .links
            .into_iter()
            .enumerate()
            .map(|(index, link)| proto_to_link(link, record_index, index))
            .collect::<Result<Vec<_>, _>>()?,
        resources: record
            .resources
            .into_iter()
            .enumerate()
            .map(|(index, resource)| proto_to_resource(resource, record_index, index))
            .collect::<Result<Vec<_>, _>>()?,
        dropped_children: record.dropped_children,
        truncated: record.truncated,
    })
}

fn span_to_proto(span: &SpanRecord) -> proto::TraceSpan {
    proto::TraceSpan {
        span_id: span.span_id.get(),
        parent_span_id: span.parent_span_id.map_or(0, SpanUid::get),
        name: span.name.clone(),
        kind: span_kind_code(span.kind),
        subsystem: subsystem_code(span.subsystem),
        started_at_unix_ms: span.started_at_unix_ms,
        finished_at_unix_ms: span.finished_at_unix_ms,
        duration_us: span.duration_us,
        fields_json: span.fields.encoded_value(),
        fields_truncated: span.fields.truncated,
        fields_sha256: span.fields.sha256.clone().unwrap_or_default(),
        fields_original_len: usize_to_u64(span.fields.original_len),
        status: span.status.map_or(0, span_status_code),
    }
}

fn proto_to_span(
    span: proto::TraceSpan,
    record_index: usize,
    span_index: usize,
) -> Result<SpanRecord, DecodeTraceError> {
    let fields = bounded_from_proto(
        span.fields_json,
        span.fields_truncated,
        span.fields_sha256,
        span.fields_original_len,
        format!("records[{record_index}].spans[{span_index}].fields_json"),
    )?;
    let parent_span_id = (span.parent_span_id != 0).then(|| SpanUid::new(span.parent_span_id));
    let kind = span_kind_from_code(span.kind, record_index, span_index)?;
    // Subsystem is a pure projection of kind (the closed `SpanKind::subsystem`
    // mapping). Derive it here rather than trusting the redundant wire byte, so
    // the codec cannot reconstruct a kind/subsystem pair that contradicts the
    // mapping the audit contract relies on.
    Ok(SpanRecord {
        span_id: SpanUid::new(span.span_id),
        parent_span_id,
        name: span.name,
        kind,
        subsystem: kind.subsystem(),
        started_at_unix_ms: span.started_at_unix_ms,
        finished_at_unix_ms: span.finished_at_unix_ms,
        duration_us: span.duration_us,
        fields,
        status: span_status_from_code(span.status, record_index, span_index)?,
    })
}

fn event_to_proto(event: &EventRecord) -> proto::TraceEvent {
    proto::TraceEvent {
        span_id: event.span_id.get(),
        name: event.name.clone(),
        module: event.module.clone(),
        at_unix_ms: event.at_unix_ms,
        details_json: event.details.encoded_value(),
        details_truncated: event.details.truncated,
        details_sha256: event.details.sha256.clone().unwrap_or_default(),
        details_original_len: usize_to_u64(event.details.original_len),
    }
}

fn proto_to_event(
    event: proto::TraceEvent,
    record_index: usize,
    event_index: usize,
) -> Result<EventRecord, DecodeTraceError> {
    Ok(EventRecord {
        span_id: SpanUid::new(event.span_id),
        name: event.name,
        module: event.module,
        at_unix_ms: event.at_unix_ms,
        details: bounded_from_proto(
            event.details_json,
            event.details_truncated,
            event.details_sha256,
            event.details_original_len,
            format!("records[{record_index}].events[{event_index}].details_json"),
        )?,
    })
}

fn link_to_proto(link: &TraceLink) -> proto::TraceLink {
    proto::TraceLink {
        kind: trace_link_kind_code(link.kind),
        value: link.value.clone(),
    }
}

fn proto_to_link(
    link: proto::TraceLink,
    record_index: usize,
    link_index: usize,
) -> Result<TraceLink, DecodeTraceError> {
    if link.value.is_empty() {
        return Err(invalid(format!(
            "records[{record_index}].links[{link_index}].value is empty"
        )));
    }
    Ok(TraceLink {
        kind: trace_link_kind_from_code(link.kind, record_index, link_index)?,
        value: link.value,
    })
}

fn resource_to_proto(resource: &ResourceStats) -> proto::TraceResource {
    proto::TraceResource {
        stats_kind: resource.meta.stats_kind.as_str().to_owned(),
        phase: resource.meta.phase.clone().unwrap_or_default(),
        source: resource.meta.source.clone(),
        source_available: resource.meta.source_available,
        read_error: resource.meta.read_error.clone().unwrap_or_default(),
        parse_error: resource.meta.parse_error.clone().unwrap_or_default(),
        sampler_duration_us: resource.meta.sampler_duration_us,
        inflight_requests: resource.meta.inflight_requests,
        payload_json: resource.payload.encoded_value(),
        payload_truncated: resource.payload.truncated,
        payload_sha256: resource.payload.sha256.clone().unwrap_or_default(),
        payload_original_len: usize_to_u64(resource.payload.original_len),
        span_id: resource.span_id.map_or(0, SpanUid::get),
    }
}

fn proto_to_resource(
    resource: proto::TraceResource,
    record_index: usize,
    resource_index: usize,
) -> Result<ResourceStats, DecodeTraceError> {
    Ok(ResourceStats {
        span_id: (resource.span_id != 0).then(|| SpanUid::new(resource.span_id)),
        meta: ResourceStatsMeta {
            stats_kind: resource_stats_kind_from_label(
                &resource.stats_kind,
                record_index,
                resource_index,
            )?,
            phase: empty_to_none(resource.phase),
            source: resource.source,
            source_available: resource.source_available,
            read_error: empty_to_none(resource.read_error),
            parse_error: empty_to_none(resource.parse_error),
            sampler_duration_us: resource.sampler_duration_us,
            inflight_requests: resource.inflight_requests,
        },
        payload: bounded_from_proto(
            resource.payload_json,
            resource.payload_truncated,
            resource.payload_sha256,
            resource.payload_original_len,
            format!("records[{record_index}].resources[{resource_index}].payload_json"),
        )?,
    })
}

fn bounded_from_proto(
    json: String,
    truncated: bool,
    sha256: String,
    original_len: u64,
    context: impl Into<String>,
) -> Result<BoundedJson, DecodeTraceError> {
    let context = context.into();
    Ok(BoundedJson {
        value: serde_json::from_str(&json).map_err(|err| invalid(format!("{context}: {err}")))?,
        truncated,
        sha256: empty_to_none(sha256),
        original_len: usize::try_from(original_len)
            .map_err(|err| invalid(format!("{context}.original_len: {err}")))?,
    })
}

fn empty_to_none(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn invalid(message: impl Into<String>) -> DecodeTraceError {
    DecodeTraceError::Invalid(message.into())
}

fn trace_kind_code(kind: TraceKind) -> i32 {
    match kind {
        TraceKind::OpRequest => 1,
        TraceKind::CommandFinalize => 2,
        TraceKind::ActiveCommandAdvance => 3,
        TraceKind::IdleWorkspaceEvict => 4,
        TraceKind::PluginService => 5,
    }
}

fn trace_kind_from_code(code: i32, record_index: usize) -> Result<TraceKind, DecodeTraceError> {
    Ok(match code {
        1 => TraceKind::OpRequest,
        2 => TraceKind::CommandFinalize,
        3 => TraceKind::ActiveCommandAdvance,
        4 => TraceKind::IdleWorkspaceEvict,
        5 => TraceKind::PluginService,
        _ => {
            return Err(invalid(format!(
                "records[{record_index}].kind has unknown code {code}"
            )))
        }
    })
}

fn span_kind_code(kind: SpanKind) -> i32 {
    match kind {
        SpanKind::OpRequest => 1,
        SpanKind::GatewayTransport => 2,
        SpanKind::GatewayRoute => 3,
        SpanKind::HostProtocol => 4,
        SpanKind::HostTransport => 5,
        SpanKind::DaemonTransport => 6,
        SpanKind::Dispatch => 7,
        SpanKind::Operation => 8,
        SpanKind::LayerStack => 9,
        SpanKind::Occ => 10,
        SpanKind::Overlay => 11,
        SpanKind::CommandProcessSpawn => 12,
        SpanKind::CommandProcessWait => 13,
        SpanKind::CommandFinalize => 14,
        SpanKind::WorkspaceRoute => 15,
        SpanKind::IsolatedWorkspace => 16,
        SpanKind::Plugin => 17,
        SpanKind::File => 18,
        SpanKind::Checkpoint => 19,
        SpanKind::Resource => 20,
        SpanKind::Control => 21,
    }
}

fn span_kind_from_code(
    code: i32,
    record_index: usize,
    span_index: usize,
) -> Result<SpanKind, DecodeTraceError> {
    Ok(match code {
        1 => SpanKind::OpRequest,
        2 => SpanKind::GatewayTransport,
        3 => SpanKind::GatewayRoute,
        4 => SpanKind::HostProtocol,
        5 => SpanKind::HostTransport,
        6 => SpanKind::DaemonTransport,
        7 => SpanKind::Dispatch,
        8 => SpanKind::Operation,
        9 => SpanKind::LayerStack,
        10 => SpanKind::Occ,
        11 => SpanKind::Overlay,
        12 => SpanKind::CommandProcessSpawn,
        13 => SpanKind::CommandProcessWait,
        14 => SpanKind::CommandFinalize,
        15 => SpanKind::WorkspaceRoute,
        16 => SpanKind::IsolatedWorkspace,
        17 => SpanKind::Plugin,
        18 => SpanKind::File,
        19 => SpanKind::Checkpoint,
        20 => SpanKind::Resource,
        21 => SpanKind::Control,
        _ => {
            return Err(invalid(format!(
                "records[{record_index}].spans[{span_index}].kind has unknown code {code}"
            )))
        }
    })
}

fn subsystem_code(subsystem: SpanSubsystem) -> i32 {
    match subsystem {
        SpanSubsystem::Wire => 1,
        SpanSubsystem::Dispatch => 2,
        SpanSubsystem::Op => 3,
        SpanSubsystem::LayerStack => 4,
        SpanSubsystem::Overlay => 5,
        SpanSubsystem::Command => 6,
        SpanSubsystem::Workspace => 7,
        SpanSubsystem::Plugin => 8,
        SpanSubsystem::Control => 9,
    }
}

fn span_status_code(status: SpanStatus) -> i32 {
    match status {
        SpanStatus::Ok => 1,
        SpanStatus::Rejected => 2,
        SpanStatus::Cancelled => 3,
        SpanStatus::TimedOut => 4,
        SpanStatus::Error => 5,
    }
}

fn span_status_from_code(
    code: i32,
    record_index: usize,
    span_index: usize,
) -> Result<Option<SpanStatus>, DecodeTraceError> {
    if code == 0 {
        return Ok(None);
    }
    Ok(Some(match code {
        1 => SpanStatus::Ok,
        2 => SpanStatus::Rejected,
        3 => SpanStatus::Cancelled,
        4 => SpanStatus::TimedOut,
        5 => SpanStatus::Error,
        _ => {
            return Err(invalid(format!(
                "records[{record_index}].spans[{span_index}].status has unknown code {code}"
            )))
        }
    }))
}

fn trace_link_kind_code(kind: TraceLinkKind) -> i32 {
    match kind {
        TraceLinkKind::Command => 1,
        TraceLinkKind::WorkspaceHandle => 2,
        TraceLinkKind::PluginService => 3,
        TraceLinkKind::ManifestVersion => 4,
    }
}

fn trace_link_kind_from_code(
    code: i32,
    record_index: usize,
    link_index: usize,
) -> Result<TraceLinkKind, DecodeTraceError> {
    Ok(match code {
        1 => TraceLinkKind::Command,
        2 => TraceLinkKind::WorkspaceHandle,
        3 => TraceLinkKind::PluginService,
        4 => TraceLinkKind::ManifestVersion,
        _ => {
            return Err(invalid(format!(
                "records[{record_index}].links[{link_index}].kind has unknown code {code}"
            )))
        }
    })
}

fn resource_stats_kind_from_label(
    label: &str,
    record_index: usize,
    resource_index: usize,
) -> Result<ResourceStatsKind, DecodeTraceError> {
    Ok(match label {
        "tree" => ResourceStatsKind::Tree,
        "host" => ResourceStatsKind::Host,
        "mount_cost" => ResourceStatsKind::MountCost,
        "cgroup_process" => ResourceStatsKind::CgroupProcess,
        _ => {
            return Err(invalid(format!(
                "records[{record_index}].resources[{resource_index}].stats_kind has unknown label {label:?}"
            )))
        }
    })
}

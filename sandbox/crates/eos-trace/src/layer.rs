use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Number, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::budget::{BoundedJson, DetailBudget};
use crate::ids::{RequestId, SpanUid, TraceId};
use crate::record::{EventRecord, SpanKind, SpanRecord, SpanStatus, TraceKind, TraceRecord};

#[derive(Debug, Clone)]
pub struct TraceSpoolLayer {
    state: Arc<Mutex<TraceLayerState>>,
    next_span_uid: Arc<AtomicU64>,
}

impl TraceSpoolLayer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TraceLayerState::default())),
            next_span_uid: Arc::new(AtomicU64::new(1)),
        }
    }

    #[must_use]
    pub fn take_finished(&self, trace_id: &TraceId) -> Option<TraceRecord> {
        self.state
            .lock()
            .expect("trace layer state mutex poisoned")
            .finished
            .remove(trace_id)
    }
}

impl Default for TraceSpoolLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for TraceSpoolLayer
where
    S: Subscriber,
    S: for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = JsonFieldVisitor::default();
        attrs.record(&mut visitor);
        let parent = parent_local(attrs, &ctx);
        let trace_id = visitor
            .string_field("trace_id")
            .and_then(|value| TraceId::parse(value).ok())
            .or_else(|| parent.as_ref().map(|parent| parent.trace_id.clone()))
            .unwrap_or_default();
        let request_id = visitor
            .string_field("request_id")
            .and_then(|value| RequestId::parse(value).ok())
            .or_else(|| parent.as_ref().and_then(|parent| parent.request_id.clone()));
        let parent_span_id = parent.as_ref().map(|parent| parent.span_id);
        let name = attrs.metadata().name().to_owned();
        let kind = visitor
            .string_field("span_kind")
            .and_then(|value| SpanKind::parse_label(&value))
            .or_else(|| SpanKind::parse_label(&name))
            .unwrap_or(SpanKind::Operation);
        let trace_exempt = visitor.bool_field("trace_exempt").unwrap_or(false);
        let span_id = SpanUid::new(self.next_span_uid.fetch_add(1, Ordering::Relaxed));
        let started_at_unix_ms = unix_ms();

        let active = ActiveSpan {
            span_id,
            parent_span_id,
            trace_id: trace_id.clone(),
            request_id: request_id.clone(),
            name,
            kind,
            fields: visitor.fields,
            started_at_unix_ms,
            started_at: Instant::now(),
            trace_exempt,
        };

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanLocal {
                span_id,
                trace_id,
                request_id,
                trace_exempt,
            });
        }
        self.state
            .lock()
            .expect("trace layer state mutex poisoned")
            .active
            .insert(span_id, active);
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let Some(local) = span.extensions().get::<SpanLocal>().cloned() else {
            return;
        };
        let mut visitor = JsonFieldVisitor::default();
        values.record(&mut visitor);
        let mut state = self.state.lock().expect("trace layer state mutex poisoned");
        let Some(active) = state.active.get_mut(&local.span_id) else {
            return;
        };
        active.fields.extend(visitor.fields);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        let Some(span) = scope.from_root().last() else {
            return;
        };
        let Some(local) = span.extensions().get::<SpanLocal>().cloned() else {
            return;
        };
        if local.trace_exempt {
            return;
        }
        let mut visitor = JsonFieldVisitor::default();
        event.record(&mut visitor);
        let name = visitor
            .string_field("event")
            .unwrap_or_else(|| event.metadata().name().to_owned());
        let details =
            BoundedJson::capture(Value::Object(visitor.fields), DetailBudget::EventDetails);
        let record = EventRecord {
            span_id: local.span_id,
            name,
            module: event.metadata().target().to_owned(),
            at_unix_ms: unix_ms(),
            details,
        };
        self.state
            .lock()
            .expect("trace layer state mutex poisoned")
            .events
            .entry(local.trace_id)
            .or_default()
            .push(record);
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let Some(local) = span.extensions().get::<SpanLocal>().cloned() else {
            return;
        };
        let finished_at_unix_ms = unix_ms();
        let mut state = self.state.lock().expect("trace layer state mutex poisoned");
        let Some(active) = state.active.remove(&local.span_id) else {
            return;
        };

        let status = active
            .fields
            .get("status")
            .and_then(Value::as_str)
            .and_then(SpanStatus::parse_label);
        let span_record = SpanRecord {
            span_id: active.span_id,
            parent_span_id: active.parent_span_id,
            name: active.name,
            kind: active.kind,
            subsystem: active.kind.subsystem(),
            started_at_unix_ms: active.started_at_unix_ms,
            finished_at_unix_ms,
            duration_us: duration_us(active.started_at.elapsed()),
            fields: BoundedJson::capture(Value::Object(active.fields), DetailBudget::SpanFields),
            status,
        };

        if active.trace_exempt {
            return;
        }

        let trace_id = active.trace_id.clone();
        state
            .spans
            .entry(trace_id.clone())
            .or_default()
            .push(span_record);

        if active.parent_span_id.is_none() {
            let mut spans = state.spans.remove(&trace_id).unwrap_or_default();
            spans.sort_by_key(|span| span.span_id);
            let events = state.events.remove(&trace_id).unwrap_or_default();
            let started_at_unix_ms = spans
                .iter()
                .map(|span| span.started_at_unix_ms)
                .min()
                .unwrap_or(finished_at_unix_ms);
            let mut record = TraceRecord::new(trace_id.clone(), active.span_id);
            record.request_id = active.request_id;
            record.kind = TraceKind::OpRequest;
            record.started_at_unix_ms = started_at_unix_ms;
            record.finished_at_unix_ms = finished_at_unix_ms;
            record.spans = spans;
            record.events = events;
            state.finished.insert(trace_id, record);
        }
    }
}

#[derive(Debug, Clone)]
struct SpanLocal {
    span_id: SpanUid,
    trace_id: TraceId,
    request_id: Option<RequestId>,
    trace_exempt: bool,
}

#[derive(Debug)]
struct ActiveSpan {
    span_id: SpanUid,
    parent_span_id: Option<SpanUid>,
    trace_id: TraceId,
    request_id: Option<RequestId>,
    name: String,
    kind: SpanKind,
    fields: Map<String, Value>,
    started_at_unix_ms: u64,
    started_at: Instant,
    trace_exempt: bool,
}

#[derive(Debug, Default)]
struct TraceLayerState {
    active: BTreeMap<SpanUid, ActiveSpan>,
    spans: BTreeMap<TraceId, Vec<SpanRecord>>,
    events: BTreeMap<TraceId, Vec<EventRecord>>,
    finished: BTreeMap<TraceId, TraceRecord>,
}

#[derive(Default)]
struct JsonFieldVisitor {
    fields: Map<String, Value>,
}

impl JsonFieldVisitor {
    fn string_field(&self, name: &str) -> Option<String> {
        self.fields
            .get(name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    fn bool_field(&self, name: &str) -> Option<bool> {
        self.fields.get(name).and_then(Value::as_bool)
    }
}

impl Visit for JsonFieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), Value::String(format!("{value:?}")));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), Value::Number(Number::from(value)));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), Value::Number(Number::from(value)));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), Value::Bool(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), Value::String(value.to_owned()));
    }
}

fn parent_local<S>(attrs: &tracing::span::Attributes<'_>, ctx: &Context<'_, S>) -> Option<SpanLocal>
where
    S: Subscriber,
    S: for<'lookup> LookupSpan<'lookup>,
{
    let parent_id = attrs
        .parent()
        .cloned()
        .or_else(|| ctx.current_span().id().cloned())?;
    ctx.span(&parent_id)
        .and_then(|span| span.extensions().get::<SpanLocal>().cloned())
}

fn unix_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn duration_us(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

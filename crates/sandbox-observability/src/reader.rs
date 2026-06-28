//! Read side: one private `scan()` primitive over primary + rotated logs, and
//! thin public folds (`trace`/`samples`/`events`/`raw`) over it. The sort is
//! intrinsic — within a file `ts` is not monotonic (a parent span is appended
//! after its children), so every reader must sort by `ts` anyway; doing it once
//! in `scan()` is strictly simpler.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::record::{Attrs, Event, Record, Span, COUNTERS_METRIC_KEY};
use crate::unix_now_ms;

/// Reader over the one append-only log and its single rotated sibling.
pub struct Reader {
    primary: PathBuf,
    rotated: PathBuf,
}

/// Owned filter for the verbatim/event folds. `Default` lets call sites write
/// `RawFilter { kind: Some("event".into()), ..Default::default() }`.
#[derive(Default, Clone, Debug)]
pub struct RawFilter {
    pub kind: Option<String>,
    pub name: Option<String>,
    pub trace: Option<String>,
    pub since_ms: i64,
}

/// One node of a `trace` forest: a span plus its start offset, child spans, and
/// the events that attach directly under it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpanNode {
    pub span: Span,
    /// `(ts - dur_ms) - trace_start`, in ms.
    pub offset_ms: f64,
    pub children: Vec<SpanNode>,
    pub events: Vec<EventNode>,
}

/// An event positioned within a trace by its offset from the trace start.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventNode {
    /// `event.ts - trace_start`, in ms.
    pub offset_ms: f64,
    pub event: Event,
}

/// One windowed sample with read-time deltas for the keys the emitter tagged as
/// counters (via the reserved `_counters` metric). Gauges and identity metrics
/// carry no delta.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleDelta {
    pub ts: i64,
    pub scope: String,
    /// Raw metric values with the internal `_counters` tag stripped; a
    /// `_truncated` marker (set when the sample line was capped) is preserved as
    /// the truncation signal.
    pub metrics: Attrs,
    /// Counter deltas versus the previous in-window sample of this scope.
    pub deltas: Attrs,
    /// `ts - previous.ts`; `None` for the first in-window sample.
    pub sample_delta_ms: Option<i64>,
}

impl Reader {
    #[must_use]
    pub fn new(primary: PathBuf, rotated: PathBuf) -> Self {
        Self { primary, rotated }
    }

    /// The one primitive: parse every line of rotated + primary, skip malformed
    /// lines, keep the verbatim line beside each record, and sort by `ts`.
    fn scan(&self) -> Vec<(Record, String)> {
        let mut scanned = Vec::new();
        for path in [&self.rotated, &self.primary] {
            let Ok(contents) = fs::read_to_string(path) else {
                continue;
            };
            for line in contents.lines() {
                if let Ok(record) = serde_json::from_str::<Record>(line) {
                    scanned.push((record, line.to_owned()));
                }
            }
        }
        scanned.sort_by_key(|(record, _)| record_ts(record));
        scanned
    }

    /// One flow as a span forest: filter by `trace`, build the tree by
    /// `span`/`parent`, order siblings by start (`ts - dur_ms`), offset each node
    /// from the trace start, and attach events under their `parent`. Resolves by
    /// id, never by append order.
    #[must_use]
    pub fn trace(&self, id: &str) -> Vec<SpanNode> {
        let mut spans = Vec::new();
        let mut events = Vec::new();
        for (record, _) in self.scan() {
            match record {
                Record::Span(span) if span.trace == id => spans.push(span),
                Record::Event(event) if event.trace == id => events.push(event),
                _ => {}
            }
        }
        build_trace_forest(spans, events)
    }

    /// Per-scope sample series with read-time counter deltas, within
    /// `now - window_ms`.
    #[must_use]
    pub fn samples(&self, scope: &str, window_ms: i64) -> Vec<SampleDelta> {
        let since = unix_now_ms().saturating_sub(window_ms);
        let in_window: Vec<(i64, Attrs)> = self
            .scan()
            .into_iter()
            .filter_map(|(record, _)| match record {
                Record::Sample(sample) if sample.scope == scope && sample.ts >= since => {
                    Some((sample.ts, sample.metrics))
                }
                _ => None,
            })
            .collect();
        sample_deltas(scope, in_window)
    }

    /// Parsed `Event` records selected by the same filter shape as `raw`,
    /// reusing `scan()`'s already-parsed records rather than re-parsing lines.
    #[must_use]
    pub fn events(&self, filter: RawFilter) -> Vec<Event> {
        self.scan()
            .into_iter()
            .filter_map(|(record, _)| match record {
                Record::Event(event) if event_matches(&event, &filter) => Some(event),
                _ => None,
            })
            .collect()
    }

    /// Verbatim lines kept by `scan()`, filtered by `kind`/`name`/`trace`/`since`.
    #[must_use]
    pub fn raw(&self, filter: RawFilter) -> Vec<String> {
        self.scan()
            .into_iter()
            .filter(|(record, _)| raw_matches(record, &filter))
            .map(|(_, line)| line)
            .collect()
    }
}

fn record_ts(record: &Record) -> i64 {
    match record {
        Record::Span(span) => span.ts,
        Record::Event(event) => event.ts,
        Record::Sample(sample) => sample.ts,
    }
}

fn record_kind(record: &Record) -> &'static str {
    match record {
        Record::Span(_) => "span",
        Record::Event(_) => "event",
        Record::Sample(_) => "sample",
    }
}

fn record_name(record: &Record) -> Option<&str> {
    match record {
        Record::Span(span) => Some(&span.name),
        Record::Event(event) => Some(&event.name),
        Record::Sample(_) => None,
    }
}

fn record_trace(record: &Record) -> Option<&str> {
    match record {
        Record::Span(span) => Some(&span.trace),
        Record::Event(event) => Some(&event.trace),
        Record::Sample(_) => None,
    }
}

fn raw_matches(record: &Record, filter: &RawFilter) -> bool {
    if record_ts(record) < filter.since_ms {
        return false;
    }
    if let Some(kind) = &filter.kind {
        if record_kind(record) != kind {
            return false;
        }
    }
    if let Some(name) = &filter.name {
        if record_name(record) != Some(name.as_str()) {
            return false;
        }
    }
    if let Some(trace) = &filter.trace {
        if record_trace(record) != Some(trace.as_str()) {
            return false;
        }
    }
    true
}

fn event_matches(event: &Event, filter: &RawFilter) -> bool {
    if event.ts < filter.since_ms {
        return false;
    }
    if let Some(name) = &filter.name {
        if event.name != name.as_str() {
            return false;
        }
    }
    if let Some(trace) = &filter.trace {
        if event.trace != trace.as_str() {
            return false;
        }
    }
    true
}

fn span_start(span: &Span) -> f64 {
    span.ts as f64 - span.dur_ms
}

/// The parent key a span attaches under within its own trace: its `parent` when
/// that parent is itself present in the trace, otherwise `None` — a span whose
/// parent lies outside this trace is re-rooted, not dropped.
fn in_trace_parent(span: &Span, span_ids: &std::collections::HashSet<String>) -> Option<String> {
    match &span.parent {
        Some(parent) if span_ids.contains(parent) => Some(parent.clone()),
        _ => None,
    }
}

fn build_trace_forest(spans: Vec<Span>, events: Vec<Event>) -> Vec<SpanNode> {
    if spans.is_empty() {
        return Vec::new();
    }
    let trace_start = spans.iter().map(span_start).fold(f64::INFINITY, f64::min);

    let mut events_by_parent: std::collections::HashMap<Option<String>, Vec<Event>> =
        std::collections::HashMap::new();
    for event in events {
        events_by_parent
            .entry(event.parent.clone())
            .or_default()
            .push(event);
    }

    let span_ids: std::collections::HashSet<String> =
        spans.iter().map(|span| span.span.clone()).collect();
    let mut children_by_parent: std::collections::HashMap<Option<String>, Vec<Span>> =
        std::collections::HashMap::new();
    for span in spans {
        let key = in_trace_parent(&span, &span_ids);
        children_by_parent.entry(key).or_default().push(span);
    }

    build_nodes(
        None,
        trace_start,
        &mut children_by_parent,
        &mut events_by_parent,
    )
}

fn build_nodes(
    parent: Option<&str>,
    trace_start: f64,
    children_by_parent: &mut std::collections::HashMap<Option<String>, Vec<Span>>,
    events_by_parent: &mut std::collections::HashMap<Option<String>, Vec<Event>>,
) -> Vec<SpanNode> {
    let key = parent.map(str::to_owned);
    let mut spans = children_by_parent.remove(&key).unwrap_or_default();
    spans.sort_by(|a, b| span_start(a).total_cmp(&span_start(b)));
    spans
        .into_iter()
        .map(|span| {
            let span_id = span.span.clone();
            let children = build_nodes(
                Some(&span_id),
                trace_start,
                children_by_parent,
                events_by_parent,
            );
            let mut events = events_by_parent.remove(&Some(span_id)).unwrap_or_default();
            events.sort_by_key(|event| event.ts);
            let events = events
                .into_iter()
                .map(|event| EventNode {
                    offset_ms: event.ts as f64 - trace_start,
                    event,
                })
                .collect();
            SpanNode {
                offset_ms: span_start(&span) - trace_start,
                span,
                children,
                events,
            }
        })
        .collect()
}

fn counter_keys(metrics: &Attrs) -> Vec<String> {
    metrics
        .get(COUNTERS_METRIC_KEY)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn presented_metrics(metrics: &Attrs) -> Attrs {
    metrics
        .iter()
        .filter(|(key, _)| key.as_str() != COUNTERS_METRIC_KEY)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn counter_delta(previous: &Value, current: &Value) -> Option<Value> {
    if let (Some(prev), Some(cur)) = (previous.as_i64(), current.as_i64()) {
        return Some(Value::from(cur - prev));
    }
    if let (Some(prev), Some(cur)) = (previous.as_f64(), current.as_f64()) {
        return Some(Value::from(cur - prev));
    }
    None
}

fn sample_deltas(scope: &str, samples: Vec<(i64, Attrs)>) -> Vec<SampleDelta> {
    let mut series = Vec::with_capacity(samples.len());
    let mut previous: Option<(i64, Attrs)> = None;
    for (ts, metrics) in samples {
        let mut deltas = Attrs::new();
        let mut sample_delta_ms = None;
        if let Some((prev_ts, prev_metrics)) = &previous {
            sample_delta_ms = Some(ts - prev_ts);
            for key in counter_keys(&metrics) {
                if let (Some(prev_value), Some(cur_value)) =
                    (prev_metrics.get(&key), metrics.get(&key))
                {
                    if let Some(delta) = counter_delta(prev_value, cur_value) {
                        deltas.insert(key, delta);
                    }
                }
            }
        }
        series.push(SampleDelta {
            ts,
            scope: scope.to_owned(),
            metrics: presented_metrics(&metrics),
            deltas,
            sample_delta_ms,
        });
        previous = Some((ts, metrics));
    }
    series
}

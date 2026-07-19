//! Streaming, bounded reads over the rotated and active event segments.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::lines::for_each_complete_line;
use crate::record::{Attrs, Event, Record, Span, SpanStatus, COUNTERS_METRIC_KEY, MAX_LINE_BYTES};
use crate::unix_now_ms;

pub const MAX_RESPONSE_RECORDS: usize = 500;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub struct Reader {
    primary: PathBuf,
    rotated: PathBuf,
    max_line_bytes: usize,
    max_records: usize,
    max_response_bytes: usize,
}

#[derive(Default, Clone, Debug)]
pub struct RawFilter {
    pub kind: Option<String>,
    pub name: Option<String>,
    pub trace: Option<String>,
    pub since_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpanNode {
    pub span: Span,
    pub offset_ms: f64,
    pub children: Vec<SpanNode>,
    pub events: Vec<EventNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventNode {
    pub offset_ms: f64,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SampleDelta {
    pub ts: i64,
    pub scope: String,
    pub metrics: Attrs,
    pub deltas: Attrs,
    pub sample_delta_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceRead {
    pub series: Vec<SampleDelta>,
    pub errors: Vec<String>,
}

struct LineEntry {
    ts: i64,
    line: String,
}

#[derive(Clone, Copy)]
struct RawLineEntry {
    ts: i64,
    sequence: usize,
    start: usize,
    len: usize,
}

/// Valid JSON records retained in one bounded byte arena. Metadata is fixed by
/// the response record cap; discarded history never owns a line allocation.
pub struct RawJsonRecords {
    storage: Vec<u8>,
    entries: Vec<RawLineEntry>,
}

impl RawJsonRecords {
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Exact JSON-array length after applying the newest-record and byte caps.
    #[must_use]
    pub fn json_array_len(&self, last_n: Option<usize>, max_bytes: usize) -> usize {
        let (_, len) = self.selection(last_n, max_bytes);
        len
    }

    /// Append a JSON array of the newest selected records without parsing them
    /// into separately allocated `Value` trees.
    pub fn write_json_array(&self, output: &mut String, last_n: Option<usize>, max_bytes: usize) {
        let (start, _) = self.selection(last_n, max_bytes);
        output.push('[');
        for (index, entry) in self.entries[start..].iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            let line = std::str::from_utf8(
                &self.storage[entry.start..entry.start.saturating_add(entry.len)],
            )
            .expect("record headers only accept UTF-8 JSON lines");
            output.push_str(line);
        }
        output.push(']');
    }

    fn selection(&self, last_n: Option<usize>, max_bytes: usize) -> (usize, usize) {
        let requested = last_n.unwrap_or(self.entries.len()).min(self.entries.len());
        let mut start = self.entries.len().saturating_sub(requested);
        let mut count = self.entries.len().saturating_sub(start);
        let mut len = 2_usize
            .saturating_add(
                self.entries[start..]
                    .iter()
                    .map(|entry| entry.len)
                    .sum::<usize>(),
            )
            .saturating_add(count.saturating_sub(1));
        while len > max_bytes && start < self.entries.len() {
            len = len.saturating_sub(self.entries[start].len);
            if count > 1 {
                len = len.saturating_sub(1);
            }
            start += 1;
            count -= 1;
        }
        (start, len.max(2))
    }
}

struct RawLineArena {
    storage: Vec<u8>,
    entries: Vec<RawLineEntry>,
    max_records: usize,
    max_bytes: usize,
    retained_bytes: usize,
    next_sequence: usize,
}

impl RawLineArena {
    fn new(max_records: usize, max_bytes: usize) -> Self {
        Self {
            storage: Vec::with_capacity(max_bytes.min(MAX_RESPONSE_BYTES)),
            entries: Vec::with_capacity(max_records.min(MAX_RESPONSE_RECORDS)),
            max_records,
            max_bytes,
            retained_bytes: 2,
            next_sequence: 0,
        }
    }

    fn insert(&mut self, ts: i64, line: &[u8]) {
        if self.max_records == 0
            || self.max_bytes < 2
            || line.len().saturating_add(2) > self.max_bytes
        {
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let candidate_bytes = line.len().saturating_add(1);
        let needs_room = self.entries.len() >= self.max_records
            || self.retained_bytes.saturating_add(candidate_bytes) > self.max_bytes;
        if needs_room
            && self
                .entries
                .iter()
                .min_by_key(|entry| (entry.ts, entry.sequence))
                .is_some_and(|oldest| (ts, sequence) < (oldest.ts, oldest.sequence))
        {
            return;
        }

        let mut reusable = None;
        while self.entries.len() >= self.max_records
            || self.retained_bytes.saturating_add(candidate_bytes) > self.max_bytes
        {
            let Some(oldest_index) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| (entry.ts, entry.sequence))
                .map(|(index, _)| index)
            else {
                return;
            };
            let removed = self.entries.remove(oldest_index);
            self.retained_bytes = self.retained_bytes.saturating_sub(removed.len + 1);
            if removed.len >= line.len()
                && reusable
                    .as_ref()
                    .is_none_or(|candidate: &RawLineEntry| removed.len < candidate.len)
            {
                reusable = Some(removed);
            }
        }

        let start = if let Some(slot) = reusable {
            self.storage[slot.start..slot.start + line.len()].copy_from_slice(line);
            slot.start
        } else {
            if self.storage.len().saturating_add(line.len()) > self.max_bytes {
                self.compact();
            }
            let start = self.storage.len();
            self.storage.extend_from_slice(line);
            start
        };
        self.entries.push(RawLineEntry {
            ts,
            sequence,
            start,
            len: line.len(),
        });
        self.retained_bytes = self.retained_bytes.saturating_add(candidate_bytes);
    }

    fn compact(&mut self) {
        self.entries.sort_by_key(|entry| entry.start);
        let mut cursor = 0;
        for entry in &mut self.entries {
            let end = entry.start.saturating_add(entry.len);
            if entry.start != cursor {
                self.storage.copy_within(entry.start..end, cursor);
                entry.start = cursor;
            }
            cursor = cursor.saturating_add(entry.len);
        }
        self.storage.truncate(cursor);
    }

    fn finish(mut self) -> RawJsonRecords {
        self.entries.sort_by_key(|entry| (entry.ts, entry.sequence));
        RawJsonRecords {
            storage: self.storage,
            entries: self.entries,
        }
    }
}

enum RecordHeader<'a> {
    Span {
        ts: i64,
        trace: Cow<'a, str>,
        parent: Option<Cow<'a, str>>,
        name: Cow<'a, str>,
        dur_ms: f64,
    },
    Event {
        ts: i64,
        trace: Cow<'a, str>,
        name: Cow<'a, str>,
    },
    Sample {
        ts: i64,
        scope: Cow<'a, str>,
    },
}

impl RecordHeader<'_> {
    fn ts(&self) -> i64 {
        match self {
            Self::Span { ts, .. } | Self::Event { ts, .. } | Self::Sample { ts, .. } => *ts,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Span { .. } => "span",
            Self::Event { .. } => "event",
            Self::Sample { .. } => "sample",
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Self::Span { name, .. } | Self::Event { name, .. } => Some(name),
            Self::Sample { .. } => None,
        }
    }

    fn trace(&self) -> Option<&str> {
        match self {
            Self::Span { trace, .. } | Self::Event { trace, .. } => Some(trace),
            Self::Sample { .. } => None,
        }
    }
}

#[derive(Deserialize)]
struct KindHeader<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
}

#[derive(Deserialize)]
struct SpanHeader<'a> {
    ts: i64,
    #[serde(borrow)]
    trace: Cow<'a, str>,
    #[serde(rename = "span", borrow)]
    _span: Cow<'a, str>,
    #[serde(default, borrow)]
    parent: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Cow<'a, str>,
    dur_ms: f64,
    #[serde(rename = "status")]
    _status: SpanStatus,
    #[serde(rename = "attrs")]
    _attrs: IgnoredObject,
}

#[derive(Deserialize)]
struct EventHeader<'a> {
    ts: i64,
    #[serde(borrow)]
    trace: Cow<'a, str>,
    #[serde(default, rename = "parent", borrow)]
    _parent: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(rename = "attrs")]
    _attrs: IgnoredObject,
}

#[derive(Deserialize)]
struct SampleHeader<'a> {
    ts: i64,
    #[serde(borrow)]
    scope: Cow<'a, str>,
}

struct IgnoredObject;

impl<'de> Deserialize<'de> for IgnoredObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = IgnoredObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(IgnoredObject)
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn record_header(line: &[u8]) -> Option<RecordHeader<'_>> {
    let kind = serde_json::from_slice::<KindHeader<'_>>(line).ok()?;
    match kind.kind.as_ref() {
        "span" => {
            let span = serde_json::from_slice::<SpanHeader<'_>>(line).ok()?;
            Some(RecordHeader::Span {
                ts: span.ts,
                trace: span.trace,
                parent: span.parent,
                name: span.name,
                dur_ms: span.dur_ms,
            })
        }
        "event" => {
            let event = serde_json::from_slice::<EventHeader<'_>>(line).ok()?;
            Some(RecordHeader::Event {
                ts: event.ts,
                trace: event.trace,
                name: event.name,
            })
        }
        "sample" => {
            let sample = serde_json::from_slice::<SampleHeader<'_>>(line).ok()?;
            Some(RecordHeader::Sample {
                ts: sample.ts,
                scope: sample.scope,
            })
        }
        _ => None,
    }
}

impl Reader {
    #[must_use]
    pub fn new(primary: PathBuf, rotated: PathBuf) -> Self {
        Self::with_limits(
            primary,
            rotated,
            MAX_LINE_BYTES,
            MAX_RESPONSE_RECORDS,
            MAX_RESPONSE_BYTES,
        )
    }

    #[must_use]
    pub fn with_limits(
        primary: PathBuf,
        rotated: PathBuf,
        max_line_bytes: usize,
        max_records: usize,
        max_response_bytes: usize,
    ) -> Self {
        Self {
            primary,
            rotated,
            max_line_bytes: max_line_bytes.min(MAX_LINE_BYTES),
            max_records: max_records.min(MAX_RESPONSE_RECORDS),
            max_response_bytes: max_response_bytes.min(MAX_RESPONSE_BYTES),
        }
    }

    #[must_use]
    pub fn trace(&self, id: &str) -> Vec<SpanNode> {
        let mut records = self
            .collect_lines(|header| header.trace() == Some(id))
            .into_iter()
            .filter_map(|entry| serde_json::from_str::<Record>(&entry.line).ok())
            .collect::<Vec<_>>();
        loop {
            let (spans, events) = split_trace_records(&records);
            let forest = build_trace_forest(spans, events);
            if serialized_len(&forest) <= self.max_response_bytes || records.is_empty() {
                return forest;
            }
            records.remove(0);
        }
    }

    #[must_use]
    pub fn latest_root_trace(&self) -> Option<String> {
        let mut latest_start = f64::NEG_INFINITY;
        let mut latest = None::<String>;
        self.for_each_header(|header, _| {
            let RecordHeader::Span {
                ts,
                trace,
                parent,
                dur_ms,
                ..
            } = header
            else {
                return;
            };
            if parent.is_some() {
                return;
            }
            let start = ts as f64 - dur_ms;
            if start > latest_start {
                latest_start = start;
                let value = latest.get_or_insert_with(String::new);
                value.clear();
                value.push_str(&trace);
            }
        });
        latest
    }

    #[must_use]
    pub fn samples(&self, scope: &str, window_ms: i64) -> Vec<SampleDelta> {
        let since = unix_now_ms().saturating_sub(window_ms);
        let values = self
            .collect_lines(|header| {
                matches!(
                    header,
                    RecordHeader::Sample {
                        ts,
                        scope: candidate,
                    } if candidate == scope && *ts >= since
                )
            })
            .into_iter()
            .filter_map(
                |entry| match serde_json::from_str::<Record>(&entry.line).ok()? {
                    Record::Sample(sample) => Some((sample.ts, sample.metrics)),
                    _ => None,
                },
            )
            .collect();
        let mut samples = sample_deltas(scope, values);
        trim_serialized(&mut samples, self.max_records, self.max_response_bytes);
        samples
    }

    #[must_use]
    pub fn resource_samples(&self, scope: &str, window_ms: i64) -> ResourceRead {
        if self.max_records == 0 || self.max_response_bytes < 2 {
            return ResourceRead {
                series: Vec::new(),
                errors: vec!["resource response limits admit no samples".to_owned()],
            };
        }
        let since = unix_now_ms().saturating_sub(window_ms);
        let mut entries = Vec::new();
        let mut retained_bytes = 2_usize;
        let mut errors = Vec::new();
        for (label, path) in [("rotated", &self.rotated), ("active", &self.primary)] {
            let mut malformed_utf8 = 0_u64;
            let mut malformed_record = 0_u64;
            let scan = for_each_complete_line(path, self.max_line_bytes, |line| {
                let Ok(text) = std::str::from_utf8(line) else {
                    malformed_utf8 = malformed_utf8.saturating_add(1);
                    return Ok(());
                };
                let Some(header) = record_header(line) else {
                    malformed_record = malformed_record.saturating_add(1);
                    return Ok(());
                };
                if matches!(
                    &header,
                    RecordHeader::Sample {
                        ts,
                        scope: candidate,
                    } if candidate == scope && *ts >= since
                ) {
                    retain_line_entry(
                        &mut entries,
                        &mut retained_bytes,
                        header.ts(),
                        text,
                        self.max_records,
                        self.max_response_bytes,
                    );
                }
                Ok(())
            });
            match scan {
                Ok(scan) => {
                    if malformed_utf8 > 0 {
                        errors.push(format!(
                            "{label} segment skipped {malformed_utf8} invalid UTF-8 lines"
                        ));
                    }
                    if malformed_record > 0 {
                        errors.push(format!(
                            "{label} segment skipped {malformed_record} malformed records"
                        ));
                    }
                    if scan.skipped_oversized > 0 {
                        errors.push(format!(
                            "{label} segment skipped {} oversized lines",
                            scan.skipped_oversized
                        ));
                    }
                    if scan.partial_tail {
                        errors.push(format!("{label} segment skipped a partial tail"));
                    }
                }
                Err(error) => errors.push(format!("{label} segment read failed: {error}")),
            }
        }
        entries.sort_by_key(|entry| entry.ts);
        let mut decode_failures = 0_u64;
        let values = entries
            .into_iter()
            .filter_map(|entry| match serde_json::from_str::<Record>(&entry.line) {
                Ok(Record::Sample(sample)) => Some((sample.ts, sample.metrics)),
                _ => {
                    decode_failures = decode_failures.saturating_add(1);
                    None
                }
            })
            .collect();
        if decode_failures > 0 {
            errors.push(format!(
                "skipped {decode_failures} undecodable sample records"
            ));
        }
        let mut series = sample_deltas(scope, values);
        trim_serialized(&mut series, self.max_records, self.max_response_bytes);
        if series.is_empty() {
            errors.push("resource store has no usable samples in the requested window".to_owned());
        }
        ResourceRead { series, errors }
    }

    #[must_use]
    pub fn latest_samples(&self, scopes: &[&str]) -> HashMap<String, SampleDelta> {
        let requested: HashSet<&str> = scopes.iter().copied().collect();
        let mut samples: HashMap<String, Vec<(i64, Attrs, usize)>> = HashMap::new();
        for entry in self.collect_lines(|header| {
            matches!(
                header,
                RecordHeader::Sample { scope, .. } if requested.contains(scope.as_ref())
            )
        }) {
            let Ok(Record::Sample(sample)) = serde_json::from_str::<Record>(&entry.line) else {
                continue;
            };
            let latest = samples.entry(sample.scope).or_default();
            latest.push((sample.ts, sample.metrics, entry.line.len() + 1));
            latest.sort_by_key(|(ts, _, _)| *ts);
            if latest.len() > 2 {
                latest.remove(0);
            }
        }
        let mut result: HashMap<String, SampleDelta> = samples
            .into_iter()
            .filter_map(|(scope, samples)| {
                sample_deltas(
                    &scope,
                    samples
                        .into_iter()
                        .map(|(ts, metrics, _)| (ts, metrics))
                        .collect(),
                )
                .pop()
                .map(|sample| (scope, sample))
            })
            .collect();
        while serialized_len(&result) > self.max_response_bytes && !result.is_empty() {
            let Some(oldest) = result
                .iter()
                .min_by_key(|(_, sample)| sample.ts)
                .map(|(scope, _)| scope.clone())
            else {
                break;
            };
            result.remove(&oldest);
        }
        result
    }

    #[must_use]
    pub fn events(&self, filter: RawFilter) -> Vec<Event> {
        let mut events: Vec<Event> = self
            .collect_lines(|header| event_header_matches(header, &filter))
            .into_iter()
            .filter_map(
                |entry| match serde_json::from_str::<Record>(&entry.line).ok()? {
                    Record::Event(event) => Some(event),
                    _ => None,
                },
            )
            .collect();
        trim_serialized(&mut events, self.max_records, self.max_response_bytes);
        events
    }

    /// Retain filtered event records in a single capped arena for direct JSON
    /// response encoding. This is the daemon query hot path; `events` remains
    /// the typed compatibility API.
    #[must_use]
    pub fn raw_json_events(&self, filter: RawFilter) -> RawJsonRecords {
        let mut records = RawLineArena::new(self.max_records, self.max_response_bytes);
        self.for_each_header(|header, line| {
            if event_header_matches(&header, &filter) {
                records.insert(header.ts(), line.as_bytes());
            }
        });
        records.finish()
    }

    #[must_use]
    pub fn raw(&self, filter: RawFilter) -> Vec<String> {
        let mut lines: Vec<String> = self
            .collect_lines(|header| raw_header_matches(header, &filter))
            .into_iter()
            .map(|entry| entry.line)
            .collect();
        trim_serialized(&mut lines, self.max_records, self.max_response_bytes);
        lines
    }

    fn collect_lines(&self, matches: impl Fn(&RecordHeader<'_>) -> bool) -> Vec<LineEntry> {
        if self.max_records == 0 || self.max_response_bytes < 2 {
            return Vec::new();
        }
        let mut entries: Vec<LineEntry> = Vec::new();
        let mut retained_bytes = 2_usize;
        self.for_each_header(|header, line| {
            if !matches(&header) {
                return;
            }
            let candidate_bytes = line.len().saturating_add(1);
            if candidate_bytes.saturating_add(2) > self.max_response_bytes {
                return;
            }

            let needs_room = entries.len() >= self.max_records
                || retained_bytes.saturating_add(candidate_bytes) > self.max_response_bytes;
            let mut reusable = None;
            if needs_room {
                let Some((oldest_index, oldest)) =
                    entries.iter().enumerate().min_by_key(|(_, entry)| entry.ts)
                else {
                    return;
                };
                if header.ts() < oldest.ts {
                    return;
                }
                let removed = entries.swap_remove(oldest_index);
                retained_bytes = retained_bytes.saturating_sub(removed.line.len() + 1);
                reusable = Some(removed.line);
            }
            while entries.len() >= self.max_records
                || retained_bytes.saturating_add(candidate_bytes) > self.max_response_bytes
            {
                let Some(oldest_index) = entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.ts)
                    .map(|(index, _)| index)
                else {
                    return;
                };
                let removed = entries.swap_remove(oldest_index);
                retained_bytes = retained_bytes.saturating_sub(removed.line.len() + 1);
            }

            let mut owned = reusable.unwrap_or_default();
            owned.clear();
            owned.push_str(line);
            entries.push(LineEntry {
                ts: header.ts(),
                line: owned,
            });
            retained_bytes = retained_bytes.saturating_add(candidate_bytes);
        });
        entries.sort_by_key(|entry| entry.ts);
        entries
    }

    fn for_each_header(&self, mut visit: impl FnMut(RecordHeader<'_>, &str)) {
        for path in [&self.rotated, &self.primary] {
            let _ = for_each_complete_line(path, self.max_line_bytes, |line| {
                let Ok(text) = std::str::from_utf8(line) else {
                    return Ok(());
                };
                let Some(header) = record_header(line) else {
                    return Ok(());
                };
                visit(header, text);
                Ok(())
            });
        }
    }
}

fn retain_line_entry(
    entries: &mut Vec<LineEntry>,
    retained_bytes: &mut usize,
    ts: i64,
    line: &str,
    max_records: usize,
    max_response_bytes: usize,
) {
    let candidate_bytes = line.len().saturating_add(1);
    if candidate_bytes.saturating_add(2) > max_response_bytes {
        return;
    }
    let needs_room = entries.len() >= max_records
        || retained_bytes.saturating_add(candidate_bytes) > max_response_bytes;
    let mut reusable = None;
    if needs_room {
        let Some((oldest_index, oldest)) =
            entries.iter().enumerate().min_by_key(|(_, entry)| entry.ts)
        else {
            return;
        };
        if ts < oldest.ts {
            return;
        }
        let removed = entries.swap_remove(oldest_index);
        *retained_bytes = retained_bytes.saturating_sub(removed.line.len() + 1);
        reusable = Some(removed.line);
    }
    while entries.len() >= max_records
        || retained_bytes.saturating_add(candidate_bytes) > max_response_bytes
    {
        let Some(oldest_index) = entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.ts)
            .map(|(index, _)| index)
        else {
            return;
        };
        let removed = entries.swap_remove(oldest_index);
        *retained_bytes = retained_bytes.saturating_sub(removed.line.len() + 1);
    }
    let mut owned = reusable.unwrap_or_default();
    owned.clear();
    owned.push_str(line);
    entries.push(LineEntry { ts, line: owned });
    *retained_bytes = retained_bytes.saturating_add(candidate_bytes);
}

fn serialized_len(value: &impl Serialize) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

fn trim_serialized<T: Serialize>(values: &mut Vec<T>, max_records: usize, max_bytes: usize) {
    while (values.len() > max_records || serialized_len(values) > max_bytes) && !values.is_empty() {
        values.remove(0);
    }
}

fn split_trace_records(records: &[Record]) -> (Vec<Span>, Vec<Event>) {
    let mut spans = Vec::new();
    let mut events = Vec::new();
    for record in records {
        match record {
            Record::Span(span) => spans.push(span.clone()),
            Record::Event(event) => events.push(event.clone()),
            Record::Sample(_) => {}
        }
    }
    (spans, events)
}

fn raw_header_matches(header: &RecordHeader<'_>, filter: &RawFilter) -> bool {
    header.ts() >= filter.since_ms
        && filter
            .kind
            .as_ref()
            .is_none_or(|kind| header.kind() == kind)
        && filter
            .name
            .as_ref()
            .is_none_or(|name| header.name() == Some(name.as_str()))
        && filter
            .trace
            .as_ref()
            .is_none_or(|trace| header.trace() == Some(trace.as_str()))
}

fn event_header_matches(header: &RecordHeader<'_>, filter: &RawFilter) -> bool {
    matches!(header, RecordHeader::Event { .. })
        && header.ts() >= filter.since_ms
        && filter
            .name
            .as_ref()
            .is_none_or(|name| header.name() == Some(name.as_str()))
        && filter
            .trace
            .as_ref()
            .is_none_or(|trace| header.trace() == Some(trace.as_str()))
}

fn span_start(span: &Span) -> f64 {
    span.ts as f64 - span.dur_ms
}

fn in_trace_parent(span: &Span, span_ids: &HashSet<String>) -> Option<String> {
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
    let mut events_by_parent: HashMap<Option<String>, Vec<Event>> = HashMap::new();
    for event in events {
        events_by_parent
            .entry(event.parent.clone())
            .or_default()
            .push(event);
    }
    let span_ids: HashSet<String> = spans.iter().map(|span| span.span.clone()).collect();
    let mut children_by_parent: HashMap<Option<String>, Vec<Span>> = HashMap::new();
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
    children_by_parent: &mut HashMap<Option<String>, Vec<Span>>,
    events_by_parent: &mut HashMap<Option<String>, Vec<Event>>,
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

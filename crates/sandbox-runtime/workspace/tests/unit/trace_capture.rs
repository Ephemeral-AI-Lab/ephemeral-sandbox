use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

static TRACE_CAPTURE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct TraceCapture {
    next_id: Arc<AtomicU64>,
    records: Arc<Mutex<Vec<String>>>,
}

impl Default for TraceCapture {
    fn default() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            records: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl TraceCapture {
    fn output(&self) -> String {
        self.records.lock().expect("trace lock").join("\n")
    }

    fn push(&self, line: String) {
        self.records.lock().expect("trace lock").push(line);
    }
}

impl Subscriber for TraceCapture {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut visitor = TextVisitor::new(format!("span {}", attrs.metadata().name()));
        attrs.record(&mut visitor);
        self.push(visitor.finish());
        Id::from_u64(id)
    }

    fn record(&self, _span: &Id, values: &Record<'_>) {
        let mut visitor = TextVisitor::new("record".to_owned());
        values.record(&mut visitor);
        self.push(visitor.finish());
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = TextVisitor::new(format!("event {}", event.metadata().name()));
        event.record(&mut visitor);
        self.push(visitor.finish());
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct TextVisitor {
    line: String,
}

impl TextVisitor {
    fn new(line: String) -> Self {
        Self { line }
    }

    fn finish(self) -> String {
        self.line
    }

    fn push_value(&mut self, field: &Field, value: impl fmt::Display) {
        use std::fmt::Write as _;

        let _ = write!(self.line, " {}={value}", field.name());
    }
}

impl Visit for TextVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push_value(field, format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_value(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_value(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_value(field, value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_value(field, value);
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push_value(field, value);
    }
}

pub(crate) fn with_trace_capture_lock<T>(run: impl FnOnce() -> T) -> T {
    let _guard = TRACE_CAPTURE_LOCK.lock().expect("trace capture lock");
    run()
}

pub(crate) fn capture_traces(run: impl FnOnce()) -> String {
    with_trace_capture_lock(|| {
        let capture = TraceCapture::default();
        let reader = capture.clone();
        tracing::subscriber::with_default(capture, || {
            tracing::callsite::rebuild_interest_cache();
            run();
        });
        tracing::callsite::rebuild_interest_cache();
        reader.output()
    })
}

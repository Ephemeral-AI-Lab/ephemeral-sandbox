//! The emit API. One `Observer` per OS process — a cheap `Clone` handle over an
//! `Arc` core (sink + `SpanIds` + a process-shared thread-local context). The
//! daemon builds it and hands clones to the runtime, so daemon (`d-*`) and
//! runtime spans share one id sequence and one parent chain.
//!
//! Emit never fails the observed work: when disabled every method is a near-free
//! no-op, and when enabled the ordinary emit methods swallow sink errors. The
//! checked sample path is reserved for callers whose response correctness
//! depends on proving that a fresh sample was persisted.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::record::{Attrs, Event, Record, Sample, Span, SpanIds, SpanStatus, COUNTERS_METRIC_KEY};
use crate::sink::Sink;
use crate::unix_now_ms;

struct Core {
    sink: Sink,
    ids: SpanIds,
    enabled: bool,
}

thread_local! {
    static CTX: RefCell<Option<TraceContext>> = const { RefCell::new(None) };
}

/// One per-process emit handle. Cloning is the sharing: every clone holds the
/// same `Arc<Core>` (sink + `SpanIds`) and reads the same thread-local context.
#[derive(Clone)]
pub struct Observer {
    core: Arc<Core>,
}

/// Leaf-owned named gate. The daemon maps its `sandbox-config` section plus a
/// `record::proc` const into this — the leaf never imports `sandbox-config`.
pub struct ObserverConfig {
    pub proc: &'static str,
    pub enabled: bool,
}

/// The chain a child attaches under. `Arc<str>` makes the per-emit clone a
/// refcount bump, not a string copy (the trace id is the whole request id).
#[derive(Clone, Debug)]
pub struct TraceContext {
    pub trace: Arc<str>,
    pub parent: Option<Arc<str>>,
}

impl Observer {
    /// Build the one per-process core from the named gate and a sink.
    #[must_use]
    pub fn new(config: ObserverConfig, sink: Sink) -> Self {
        Self {
            core: Arc::new(Core {
                sink,
                ids: SpanIds::new(config.proc),
                enabled: config.enabled,
            }),
        }
    }

    /// A permanently-disabled observer whose every emit is a no-op. Used where no
    /// real sink exists (the daemon's observability stack is absent) and by tests
    /// that construct services directly without a trace context.
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(
            ObserverConfig {
                proc: crate::record::proc::DAEMON,
                enabled: false,
            },
            Sink::new(std::path::PathBuf::new(), crate::record::MAX_LINE_BYTES),
        )
    }

    /// Open a sync span that records on drop, nested under the thread-local
    /// parent. No enclosing context (or disabled) ⇒ a no-op guard.
    #[must_use]
    pub fn span(&self, name: &'static str) -> SpanGuard {
        if !self.core.enabled {
            return SpanGuard::noop(Arc::clone(&self.core));
        }
        let Some(parent) = self.context() else {
            return SpanGuard::noop(Arc::clone(&self.core));
        };
        let span_id = self.core.ids.next();
        let child = TraceContext {
            trace: Arc::clone(&parent.trace),
            parent: Some(Arc::from(span_id.as_str())),
        };
        let previous = CTX.with(|cell| cell.replace(Some(child)));
        SpanGuard::open(
            Arc::clone(&self.core),
            OpenGuard {
                span: span_id,
                ctx: parent,
                name,
                start_ms: unix_now_ms(),
                previous,
            },
        )
    }

    /// Run `body` inside a sync span; if it returns `Err` and the span is still
    /// `Completed`, self-set `Error` (status only — reason rides via the body's
    /// `.attr()`). An explicit `TimedOut`/`Cancelled` is not clobbered.
    pub fn scope<T, E>(
        &self,
        name: &'static str,
        body: impl FnOnce(&SpanGuard) -> Result<T, E>,
    ) -> Result<T, E> {
        let guard = self.span(name);
        let result = body(&guard);
        if result.is_err() && guard.is_completed() {
            guard.status(SpanStatus::Error);
        }
        result
    }

    /// Emit one event under the thread-local parent. Drops the fact if there is
    /// no current context (rather than emitting an orphan `trace=""` record).
    pub fn event(&self, name: &'static str, attrs: impl Into<Value>) {
        if !self.core.enabled {
            return;
        }
        let Some(ctx) = self.context() else {
            return;
        };
        let event = Event {
            ts: unix_now_ms(),
            trace: ctx.trace.to_string(),
            parent: ctx.parent.as_ref().map(|parent| parent.to_string()),
            name: Cow::Borrowed(name),
            attrs: value_to_attrs(attrs.into()),
        };
        let _ = self.core.sink.append(&Record::Event(event));
    }

    /// Emit one resource/metric sample. Samples carry no trace/parent.
    pub fn sample(&self, scope: &str, metrics: impl Into<Value>) {
        let _ = self.try_sample(scope, metrics);
    }

    /// Persist one resource/metric sample, returning a sink error to callers
    /// that must not answer from an older sample when the append fails.
    pub fn try_sample(&self, scope: &str, metrics: impl Into<Value>) -> std::io::Result<()> {
        if !self.core.enabled {
            return Ok(());
        }
        let metrics = value_to_attrs(metrics.into());
        debug_assert!(
            metrics
                .keys()
                .all(|key| !key.starts_with('_') || key == COUNTERS_METRIC_KEY),
            "sample metric keys starting with '_' are reserved for system meta; \
             only the emit-site counter tag may be supplied by callers"
        );
        let sample = Sample {
            ts: unix_now_ms(),
            scope: scope.to_owned(),
            metrics,
        };
        self.core.sink.append(&Record::Sample(sample))
    }

    /// Snapshot the current thread-local context (e.g. to cross a thread).
    #[must_use]
    pub fn context(&self) -> Option<TraceContext> {
        CTX.with(|cell| cell.borrow().clone())
    }

    /// The append log path to hand to a forked child. Disabled observers return
    /// `None`, so child emit remains gated by the same config switch.
    #[must_use]
    pub fn log_path(&self) -> Option<std::path::PathBuf> {
        if !self.core.enabled {
            return None;
        }
        let path = self.core.sink.path();
        (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
    }

    /// Set the thread-local context for `f`, then restore the previous one —
    /// even if `f` unwinds, and even when `ctx` is `None` (which makes any
    /// span/event inside `f` no-op rather than emit an orphan).
    pub fn with_context<R>(
        &self,
        ctx: impl Into<Option<TraceContext>>,
        f: impl FnOnce() -> R,
    ) -> R {
        let previous = CTX.with(|cell| cell.replace(ctx.into()));
        let _restore = CtxRestore {
            previous: Some(previous),
        };
        f()
    }
}

struct CtxRestore {
    previous: Option<Option<TraceContext>>,
}

impl Drop for CtxRestore {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            CTX.with(|cell| {
                let _ = cell.replace(previous);
            });
        }
    }
}

struct OpenGuard {
    span: String,
    ctx: TraceContext,
    name: &'static str,
    start_ms: i64,
    previous: Option<TraceContext>,
}

/// A sync span handle: accumulates `attrs` and a `status` (default `Completed`)
/// and writes exactly one `Span` on drop. Same-thread and `!Send` — never hold
/// across `.await`; use `SpanRegistry` for work that crosses a boundary.
pub struct SpanGuard {
    core: Arc<Core>,
    open: Option<OpenGuard>,
    status: Cell<SpanStatus>,
    attrs: RefCell<Attrs>,
    _not_send: PhantomData<*const ()>,
}

impl SpanGuard {
    fn open(core: Arc<Core>, open: OpenGuard) -> Self {
        Self {
            core,
            open: Some(open),
            status: Cell::new(SpanStatus::Completed),
            attrs: RefCell::new(Attrs::new()),
            _not_send: PhantomData,
        }
    }

    fn noop(core: Arc<Core>) -> Self {
        Self {
            core,
            open: None,
            status: Cell::new(SpanStatus::Completed),
            attrs: RefCell::new(Attrs::new()),
            _not_send: PhantomData,
        }
    }

    /// Annotate a fact on a live guard (chainable). `span()` must be let-bound
    /// for this to land — annotating a temporary records a ~0 ms span.
    pub fn attr(&self, key: &'static str, value: impl Into<Value>) -> &Self {
        if self.open.is_some() {
            self.attrs.borrow_mut().insert(key.to_owned(), value.into());
        }
        self
    }

    /// Override the default `Completed` (chainable).
    pub fn status(&self, status: SpanStatus) -> &Self {
        self.status.set(status);
        self
    }

    fn is_completed(&self) -> bool {
        self.status.get() == SpanStatus::Completed
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        CTX.with(|cell| {
            let _ = cell.replace(open.previous);
        });
        let now = unix_now_ms();
        let span = Span {
            ts: now,
            trace: open.ctx.trace.to_string(),
            span: open.span,
            parent: open.ctx.parent.as_ref().map(|parent| parent.to_string()),
            name: Cow::Borrowed(open.name),
            dur_ms: (now - open.start_ms) as f64,
            status: self.status.get(),
            attrs: self.attrs.borrow().clone(),
        };
        let _ = self.core.sink.append(&Record::Span(span));
    }
}

struct OpenSpan {
    span: String,
    ctx: TraceContext,
    name: &'static str,
    start_ms: i64,
}

/// The reusable async-span store: park an open span by id, record (or cancel) it
/// by id when the work completes on another thread. One generic registry serves
/// every async source — a new source needs no bespoke map/lock.
pub struct SpanRegistry<K: Eq + Hash> {
    obs: Observer,
    open: Mutex<HashMap<K, OpenSpan>>,
}

impl<K: Eq + Hash> SpanRegistry<K> {
    /// An empty registry sharing the observer's core.
    #[must_use]
    pub fn new(obs: Observer) -> Self {
        Self {
            obs,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a span id, park the open span (self-stamping `start_ms`), and return
    /// the child context `{ trace, parent: <new id> }` the caller threads into a
    /// forked child — so a cross-process child can stamp its parent at launch.
    pub(crate) fn open(&self, id: K, ctx: TraceContext, name: &'static str) -> TraceContext {
        if !self.obs.core.enabled {
            return ctx;
        }
        let span = self.obs.core.ids.next();
        let child = TraceContext {
            trace: Arc::clone(&ctx.trace),
            parent: Some(Arc::from(span.as_str())),
        };
        lock(&self.open).insert(
            id,
            OpenSpan {
                span,
                ctx,
                name,
                start_ms: unix_now_ms(),
            },
        );
        child
    }

    /// The public launch path: `open` iff `ctx` is `Some` (passing the child
    /// context to `f`), run `f`, and `cancel` internally on `Err` — so the
    /// open↔record/cancel pairing can't be broken and a failed launch writes no
    /// bogus `cancelled`. Requires `K: Clone`: the id is parked under `open` and
    /// retained to `cancel` the same entry on `Err`.
    pub fn launch<T, E>(
        &self,
        id: K,
        ctx: Option<TraceContext>,
        name: &'static str,
        f: impl FnOnce(Option<TraceContext>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        K: Clone,
    {
        let child = ctx.map(|ctx| self.open(id.clone(), ctx, name));
        let result = f(child);
        if result.is_err() {
            self.cancel(&id);
        }
        result
    }

    /// Pop the parked span and write one completed async `Span` (`dur_ms = now -
    /// start`). No-op if `id` was never parked. Named `finish` (not `record`) to
    /// avoid colliding with the `record` module / `Record` type.
    pub fn finish(&self, id: &K, status: SpanStatus, attrs: impl Into<Value>) {
        let Some(open) = lock(&self.open).remove(id) else {
            return;
        };
        write_open_span(&self.obs, open, status, value_to_attrs(attrs.into()));
    }

    /// Pop without writing (launch failed before the work ran). No-op if `id`
    /// was never parked.
    pub(crate) fn cancel(&self, id: &K) {
        let _ = lock(&self.open).remove(id);
    }
}

impl<K: Eq + Hash> Drop for SpanRegistry<K> {
    fn drop(&mut self) {
        let mut open = lock(&self.open);
        for (_, open_span) in open.drain() {
            write_open_span(&self.obs, open_span, SpanStatus::Cancelled, Attrs::new());
        }
    }
}

fn write_open_span(obs: &Observer, open: OpenSpan, status: SpanStatus, attrs: Attrs) {
    if !obs.core.enabled {
        return;
    }
    let now = unix_now_ms();
    let span = Span {
        ts: now,
        trace: open.ctx.trace.to_string(),
        span: open.span,
        parent: open.ctx.parent.as_ref().map(|parent| parent.to_string()),
        name: Cow::Borrowed(open.name),
        dur_ms: (now - open.start_ms) as f64,
        status,
        attrs,
    };
    let _ = obs.core.sink.append(&Record::Span(span));
}

/// The engine-facing terminal edge: an async engine notifies this at completion,
/// without ever naming a span. Owned by the leaf so any execution-type crate can
/// share it; generic over the id type so owning it pulls no dependency.
pub trait TerminalHook<K>: Send + Sync {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>);
}

/// A hook that records nothing, for any engine's `K`.
pub struct NoopHook;

impl<K> TerminalHook<K> for NoopHook {
    fn on_terminal(&self, _: &K, _: SpanStatus, _: Option<i64>) {}
}

/// The registry is itself the terminal hook for any async source: it folds the
/// one generic terminal datum (`exit_code`) into the parked span and records it.
/// A new async source wires its `SpanRegistry<ItsId>` as the hook directly — no
/// adapter type and no per-source impl; the generality lives entirely in `<K>`.
/// Source-specific terminal facts ride in the `record` attrs, not a wider trait.
impl<K: Eq + Hash + Send> TerminalHook<K> for SpanRegistry<K> {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>) {
        let mut attrs = Attrs::new();
        if let Some(exit_code) = exit_code {
            attrs.insert("exit_code".to_owned(), Value::from(exit_code));
        }
        self.finish(id, status, Value::Object(attrs));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn value_to_attrs(value: Value) -> Attrs {
    match value {
        Value::Object(map) => map,
        other => {
            let mut map = Attrs::new();
            map.insert("value".to_owned(), other);
            map
        }
    }
}

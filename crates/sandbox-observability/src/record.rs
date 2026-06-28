//! The one NDJSON record model: a `span`/`event`/`sample` envelope written one
//! record per line. The enum is **internally tagged** on `kind`, so the tag
//! rides as a sibling field (`{"kind":"span", …}`) and a single `Sink::append` /
//! `Reader` scan handles every record. Records are write-internal: callers never
//! construct one directly — they go through the `Observer` emit API, which stamps
//! `ts`/`trace`/`span`/`parent`.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Open domain facts attached to a span/event, or the metric bag of a sample.
pub type Attrs = serde_json::Map<String, serde_json::Value>;

/// Whole-serialized-line byte cap — the single bound the model enforces. Only
/// `attrs`/`metrics` are unbounded; on overflow the `Sink` replaces them with a
/// `{"_truncated": <byte_len>}` marker (see `Sink::append`).
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Emit-site tag naming which `Sample.metrics` keys are monotonic counters: the
/// emitter owns the metric vocabulary and marks counter keys here, so the
/// `Reader` Δs exactly these and leaves gauges untouched. Rides in-band in the
/// flattened sample line and is stripped from the presented view. The `_` prefix
/// marks a reserved system meta-key that callers must not emit themselves.
pub const COUNTERS_METRIC_KEY: &str = "_counters";

/// Marker the `Sink` writes when a record line exceeds `MAX_LINE_BYTES`, carrying
/// the original `attrs`/`metrics` byte length. Reserved like `COUNTERS_METRIC_KEY`.
pub const TRUNCATED_KEY: &str = "_truncated";

/// Named `<proc>` tokens for the `"<proc>-<seq>"` span id (§2.3). One per OS
/// process so the daemon (`d-*`) and forked namespace-process (`np-*`) never
/// collide on one file.
pub mod proc {
    /// The daemon/runtime process token.
    pub const DAEMON: &str = "d";
    /// The forked namespace-process token.
    pub const NAMESPACE_PROCESS: &str = "np";
}

/// Grep-able, typo-safe span/event labels. The vocabulary is open (a new name is
/// one new const), but the grammar is fixed and uniform:
///
/// - **spans** = `subsystem[.area].action` (imperative) — `command.exec`,
///   `workspace_session.create`, `namespace.exec.run_shell`,
///   `namespace.exec.mount_overlay`, `layerstack.publish`.
/// - **events** = `subsystem.fact` (past-tense) — `lease.acquired`, `lease.released`.
pub mod names {
    /// Daemon request dispatch span (trace root).
    pub const DAEMON_DISPATCH: &str = "daemon.dispatch";
    /// Command execution span.
    pub const COMMAND_EXEC: &str = "command.exec";
    /// Workspace session creation span.
    pub const WORKSPACE_SESSION_CREATE: &str = "workspace_session.create";
    /// Workspace session change-capture span (one-shot finalize tail).
    pub const WORKSPACE_SESSION_CAPTURE_CHANGES: &str = "workspace_session.capture_changes";
    /// Workspace session teardown span (one-shot finalize tail).
    pub const WORKSPACE_SESSION_DESTROY: &str = "workspace_session.destroy";
    /// Namespace shell-exec span (async; recorded at child-exit).
    pub const NAMESPACE_EXEC_RUN_SHELL: &str = "namespace.exec.run_shell";
    /// Namespace overlay-mount span (sync).
    pub const NAMESPACE_EXEC_MOUNT_OVERLAY: &str = "namespace.exec.mount_overlay";
    /// Namespace-process child spawn span (cross-process).
    pub const NAMESPACE_RUNNER_SPAWN_CHILD: &str = "namespace.runner.spawn_child";
    /// Layerstack publish span.
    pub const LAYERSTACK_PUBLISH: &str = "layerstack.publish";

    /// A layer lease was acquired.
    pub const LEASE_ACQUIRED: &str = "lease.acquired";
    /// A layer lease was released.
    pub const LEASE_RELEASED: &str = "lease.released";
}

/// One NDJSON record. Internally tagged on `kind` so the tag is a sibling field,
/// not a nesting wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Record {
    /// A completed unit of work (sync or async).
    Span(Span),
    /// A point-in-time fact within a trace.
    Event(Event),
    /// A point-in-time resource/metric reading; not part of a flow.
    Sample(Sample),
}

/// A completed span: one record written at completion time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// Completion time (unix ms); start = `ts - dur_ms`.
    pub ts: i64,
    /// Trace/request id this span belongs to.
    pub trace: String,
    /// Process-unique id, `"<proc>-<seq>"`.
    pub span: String,
    /// Parent span id; `None` at the trace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Dotted label; `&'static` on write, owned on read.
    pub name: Cow<'static, str>,
    /// Span duration in milliseconds.
    pub dur_ms: f64,
    /// Closed cross-cutting outcome.
    pub status: SpanStatus,
    /// Open domain facts: `exit_code`, `op`, `one_shot`, ….
    pub attrs: Attrs,
}

/// A point-in-time fact emitted within an enclosing span/context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Emit time (unix ms).
    pub ts: i64,
    /// Trace/request id this event belongs to.
    pub trace: String,
    /// Enclosing span id; `None` only at the trace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Dotted label; `&'static` on write, owned on read.
    pub name: Cow<'static, str>,
    /// Open domain facts.
    pub attrs: Attrs,
}

/// A point-in-time resource/metric reading. Has no `trace` — samples are not
/// part of a flow. `metrics` is flattened to the top level so the layerstack
/// slice's on-disk sample bytes keep parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Sample time (unix ms).
    pub ts: i64,
    /// `"sandbox"` | `"stack"` | `"<workspace id>"`.
    pub scope: String,
    /// `cpu_usec`/`mem_cur`/`disk_bytes`/… or `layer_count`/`layers_bytes`/….
    /// Reserved `_`-prefixed keys carry meta (`_counters`, `_truncated`).
    #[serde(flatten)]
    pub metrics: Attrs,
}

/// The closed cross-cutting outcome axis the renderer color-codes. Domain
/// sub-states (`skipped`, retries) and `exit_code` ride in `attrs`, never here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    /// Finished successfully.
    Completed,
    /// Finished with a failure.
    Error,
    /// Abandoned before completion.
    Cancelled,
    /// Exceeded its deadline.
    TimedOut,
}

/// Per-process span-id allocator: mints `"<proc>-<seq>"`. One per OS process,
/// shared by every handle (held in the one per-process `Observer`'s core), so
/// the daemon (`d-*`) and namespace-process (`np-*`) never collide and the
/// daemon→runtime sequence stays monotonic.
#[derive(Debug)]
pub struct SpanIds {
    proc_token: &'static str,
    seq: AtomicU64,
}

impl SpanIds {
    /// A fresh allocator for `proc_token` (a `record::proc` const).
    #[must_use]
    pub fn new(proc_token: &'static str) -> Self {
        Self {
            proc_token,
            seq: AtomicU64::new(0),
        }
    }

    /// Mint the next process-unique span id.
    #[must_use]
    pub fn next(&self) -> String {
        format!(
            "{}-{}",
            self.proc_token,
            self.seq.fetch_add(1, Ordering::Relaxed)
        )
    }
}

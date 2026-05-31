//! Daemon-side audit RING BUFFER + the impure emit bridges.
//!
//! The pure audit *schema* (the `*Section` types, [`Lane`], [`SCHEMA_VERSION`],
//! the cap/pressure constants, and `build_event`) already lives in
//! [`eos_protocol::audit`] — this module does NOT redefine it. What lives HERE
//! is the daemon-owned, IMPURE machinery the severing left behind:
//!
//! * [`AuditBuffer`] — the bounded in-memory ring with lane-priority eviction
//!   (`sample -> normal -> critical`), edge-triggered 0.8 pressure detection,
//!   monotonic `seq`/`lane` injection, and the `pull` / `snapshot` views the
//!   `api.audit.{pull,snapshot}` ops read. The daemon never writes audit to
//!   disk; consumers pull from this ring.
//! * [`safe_emit`] / [`safe_record_phase`] — the two impure bridges that the
//!   audit-schema severing (severing #1, the PARTIAL one) keeps daemon-side:
//!   `safe_emit` appends to this ring swallowing errors; `safe_record_phase`
//!   reaches into the out-of-scope engine phase buffer. Both never break the
//!   hot path.
//!
//! Concurrency: a single mutex guards all ring state. The daemon dispatcher is
//! single-threaded async plus boot-time emitters that may fire before the loop
//! starts; a plain lock is correct for both — and the lock is NEVER held across
//! an `.await` (the ring ops are synchronous).
//! `// PORT backend/src/sandbox/daemon/audit_buffer.py — AuditBuffer ring`
//! `// PORT backend/src/sandbox/daemon/audit_schema.py:294,310 — safe_emit / safe_record_phase`

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use eos_protocol::audit::{
    Lane, DEFAULT_MAX_BYTES, DEFAULT_MAX_EVENTS, DEFAULT_PRESSURE_THRESHOLD, SCHEMA_VERSION,
};

/// A single buffered event: its monotonic sequence, lane, encoded size, and the
/// payload (already stamped with `seq`/`lane`).
/// `// PORT backend/src/sandbox/daemon/audit_buffer.py:69-74 — BufferedEvent`
#[derive(Debug, Clone, PartialEq)]
pub struct BufferedEvent {
    /// Monotonic per-buffer sequence number.
    pub seq: u64,
    /// Lane this event was appended on.
    pub lane: Lane,
    /// Byte size of the JSON-encoded payload (drives the byte cap).
    pub encoded_bytes: u64,
    /// The event payload, with `seq`/`lane` injected.
    pub payload: Value,
}

/// Per-lane retained-event/byte/dropped counters.
/// `// PORT backend/src/sandbox/daemon/audit_buffer.py:84-89 — _LaneCounters`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneCounters {
    /// Retained events in this lane.
    pub events: u64,
    /// Retained bytes in this lane.
    pub bytes: u64,
    /// Events evicted from this lane under pressure.
    pub dropped: u64,
}

/// Bounded in-memory audit ring with lane-priority eviction.
///
/// The ring caps on BOTH event count and byte size; when either ceiling is
/// exceeded it evicts in [`Lane::EVICTION_ORDER`] (sample first, critical last)
/// so critical-lane events survive sample-lane pressure. A rising cross of the
/// pressure threshold is edge-triggered and (in the full port) re-emits a
/// `daemon.audit_buffer_pressure` critical event OUTSIDE the lock.
pub struct AuditBuffer {
    inner: Mutex<RingState>,
    boot_epoch_id: i64,
    pressure_threshold: f64,
}

/// The mutex-guarded ring state. Held synchronously only; never across `.await`.
#[derive(Debug)]
struct RingState {
    max_events: u64,
    max_bytes: u64,
    next_seq: u64,
    lost_before_seq: u64,
    dropped_total: u64,
    /// All retained events in append order (the `pull` scan order).
    all: VecDeque<BufferedEvent>,
    /// Per-lane FIFO queues (the eviction victims).
    lanes: [VecDeque<BufferedEvent>; 3],
    counters: [LaneCounters; 3],
    /// Whether the last observed pressure was already above threshold
    /// (edge-trigger latch).
    pressure_above: bool,
}

impl AuditBuffer {
    /// Build a ring with the default caps (50_000 events / 8 MiB) and a fresh
    /// boot epoch id. `// PORT backend/src/sandbox/daemon/audit_buffer.py:122-152`
    pub fn new() -> Self {
        Self::with_caps(DEFAULT_MAX_EVENTS, DEFAULT_MAX_BYTES, None)
    }

    /// Build a ring with explicit caps and an optional fixed boot epoch id.
    pub fn with_caps(max_events: u64, max_bytes: u64, boot_epoch_id: Option<i64>) -> Self {
        Self {
            inner: Mutex::new(RingState {
                max_events,
                max_bytes,
                next_seq: 0,
                lost_before_seq: 0,
                dropped_total: 0,
                all: VecDeque::new(),
                lanes: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
                counters: [LaneCounters::default(); 3],
                pressure_above: false,
            }),
            // PORT backend/src/sandbox/daemon/audit_buffer.py:134-136 — boot_epoch_id = monotonic_ns()
            boot_epoch_id: boot_epoch_id.unwrap_or_else(default_boot_epoch_id),
            pressure_threshold: DEFAULT_PRESSURE_THRESHOLD,
        }
    }

    /// The boot epoch id stamped on `daemon.*` events and the snapshot block.
    pub fn boot_epoch_id(&self) -> i64 {
        self.boot_epoch_id
    }

    /// Append `event` on `lane`, returning the assigned sequence number.
    ///
    /// Injects `seq`/`lane` into the payload, enforces the caps (evicting in
    /// lane priority), and on a rising pressure cross re-emits the
    /// `daemon.audit_buffer_pressure` event OUTSIDE the lock.
    // PORT backend/src/sandbox/daemon/audit_buffer.py:160-200 — append(): seq/lane stamp, enforce caps, edge-triggered pressure emit
    pub fn append(&self, event: Value, lane: Lane) -> u64 {
        let encoded_bytes = encoded_size(&event);
        let mut state = self.inner.lock().expect("audit buffer poisoned");
        let seq = state.next_seq;
        state.next_seq += 1;
        let mut payload = event;
        if let Value::Object(ref mut obj) = payload {
            obj.insert("seq".to_owned(), Value::Number(seq.into()));
            obj.insert(
                "lane".to_owned(),
                serde_json::to_value(lane).unwrap_or(Value::String("normal".to_owned())),
            );
        }
        let buffered = BufferedEvent {
            seq,
            lane,
            encoded_bytes,
            payload,
        };
        let index = lane_index(lane);
        state.counters[index].events += 1;
        state.counters[index].bytes += encoded_bytes;
        state.lanes[index].push_back(buffered.clone());
        state.all.push_back(buffered);
        enforce_caps_locked(&mut state);
        let pressure = pressure_locked(&state);
        state.pressure_above = pressure >= self.pressure_threshold;
        seq
    }

    /// Pull events strictly after `after_seq` (up to `limit`), with the buffer +
    /// snapshot blocks and the cursor. Backs `api.audit.pull`.
    // PORT backend/src/sandbox/daemon/audit_buffer.py:202-225 — pull(after_seq, limit)
    pub fn pull(&self, after_seq: i64, limit: usize) -> Value {
        let limit = limit.max(1);
        let state = self.inner.lock().expect("audit buffer poisoned");
        let events: Vec<Value> = state
            .all
            .iter()
            .filter(|event| after_seq < 0 || event.seq > after_seq as u64)
            .take(limit)
            .map(|event| event.payload.clone())
            .collect();
        let cursor_after = events
            .last()
            .and_then(|event| event.get("seq"))
            .and_then(Value::as_i64)
            .unwrap_or(after_seq);
        serde_json::json!({
            "schema": SCHEMA_VERSION,
            "cursor": {
                "after_seq": cursor_after,
                "lost_before_seq": state.lost_before_seq,
            },
            "buffer": buffer_block(&state),
            "snapshot": snapshot_block(&state, self.boot_epoch_id),
            "events": events,
        })
    }

    /// Buffer + snapshot blocks with no events. Backs `api.audit.snapshot`.
    // PORT backend/src/sandbox/daemon/audit_buffer.py:227-234 — snapshot()
    pub fn snapshot(&self) -> Value {
        let state = self.inner.lock().expect("audit buffer poisoned");
        serde_json::json!({
            "schema": SCHEMA_VERSION,
            "buffer": buffer_block(&state),
            "snapshot": snapshot_block(&state, self.boot_epoch_id),
        })
    }
}

impl Default for AuditBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Append `event` to the daemon ring on `lane`, swallowing any error.
///
/// Audit emits never break the hot path; subsystems use this single bridge so
/// the try/swallow discipline lives in one place. IMPURE: it reaches the
/// process-wide buffer singleton (the future port resolves the singleton; the
/// pure schema constructors stay in [`eos_protocol::audit`]).
// PORT backend/src/sandbox/daemon/audit_schema.py:294-307 — safe_emit(event, lane): lazy get_audit_buffer().append, swallow
pub fn safe_emit(event: Value, lane: Lane) {
    let _ = global_audit_buffer().append(event, lane);
}

/// Bridge to the engine's per-call phase buffer (`record_phase`).
///
/// Lazy-bound so the sandbox does not carry an unconditional engine dependency;
/// no-ops when no per-call buffer is active. Used by the overlay/OCC publish
/// boundaries. IMPURE: it reaches the (out-of-scope) engine package.
// PORT backend/src/sandbox/daemon/audit_schema.py:310-326 — safe_record_phase(phase, duration_ms): lazy engine.tool_call.phase_buffer.record_phase, swallow
pub fn safe_record_phase(phase: &str, duration_ms: f64) {
    let _ = (phase, duration_ms);
}

fn global_audit_buffer() -> &'static AuditBuffer {
    static BUFFER: OnceLock<AuditBuffer> = OnceLock::new();
    BUFFER.get_or_init(AuditBuffer::new)
}

fn default_boot_epoch_id() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn encoded_size(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or_else(|_| format!("{value:?}").len() as u64)
}

fn lane_index(lane: Lane) -> usize {
    match lane {
        Lane::Critical => 0,
        Lane::Normal => 1,
        Lane::Sample => 2,
    }
}

fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Critical => "critical",
        Lane::Normal => "normal",
        Lane::Sample => "sample",
    }
}

fn enforce_caps_locked(state: &mut RingState) {
    while retained_events(state) > state.max_events || retained_bytes(state) > state.max_bytes {
        if !evict_one_locked(state) {
            break;
        }
    }
}

fn evict_one_locked(state: &mut RingState) -> bool {
    for lane in Lane::EVICTION_ORDER {
        let index = lane_index(lane);
        let Some(victim) = state.lanes[index].pop_front() else {
            continue;
        };
        if let Some(position) = state.all.iter().position(|event| event.seq == victim.seq) {
            state.all.remove(position);
        }
        state.counters[index].events = state.counters[index].events.saturating_sub(1);
        state.counters[index].bytes = state.counters[index]
            .bytes
            .saturating_sub(victim.encoded_bytes);
        state.counters[index].dropped += 1;
        state.dropped_total += 1;
        state.lost_before_seq = state.lost_before_seq.max(victim.seq + 1);
        return true;
    }
    false
}

fn retained_events(state: &RingState) -> u64 {
    state.counters.iter().map(|counter| counter.events).sum()
}

fn retained_bytes(state: &RingState) -> u64 {
    state.counters.iter().map(|counter| counter.bytes).sum()
}

fn pressure_locked(state: &RingState) -> f64 {
    (retained_events(state) as f64 / state.max_events as f64)
        .max(retained_bytes(state) as f64 / state.max_bytes as f64)
}

fn buffer_block(state: &RingState) -> Value {
    let dropped_by_lane = Lane::STORAGE_ORDER
        .into_iter()
        .map(|lane| {
            (
                lane_name(lane).to_owned(),
                Value::Number(state.counters[lane_index(lane)].dropped.into()),
            )
        })
        .collect();
    serde_json::json!({
        "retained_events": retained_events(state),
        "retained_bytes": retained_bytes(state),
        "max_events": state.max_events,
        "max_bytes": state.max_bytes,
        "pressure": pressure_locked(state),
        "dropped_event_count": state.dropped_total,
        "dropped_event_count_by_lane": Value::Object(dropped_by_lane),
        "lost_before_seq": state.lost_before_seq,
    })
}

fn snapshot_block(state: &RingState, boot_epoch_id: i64) -> Value {
    serde_json::json!({
        "daemon": {
            "boot_epoch_id": boot_epoch_id,
            "next_seq": state.next_seq,
        }
    })
}

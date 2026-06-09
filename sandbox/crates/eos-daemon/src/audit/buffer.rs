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
//! * [`safe_emit`] — the impure bridge that appends to this ring while
//!   swallowing errors so audit emission never breaks the hot path.
//!
//! Concurrency: a single mutex guards all ring state. The daemon dispatcher is
//! single-threaded async plus boot-time emitters that may fire before the loop
//! starts; a plain lock is correct for both — and the lock is NEVER held across
//! an `.await` (the ring ops are synchronous).

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use eos_protocol::audit::{
    Lane, DEFAULT_MAX_BYTES, DEFAULT_MAX_EVENTS, DEFAULT_PRESSURE_THRESHOLD, SCHEMA_VERSION,
};

/// A single buffered event: its monotonic sequence, lane, encoded size, and the
/// payload (already stamped with `seq`/`lane`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BufferedEvent {
    /// Monotonic per-buffer sequence number.
    seq: u64,
    /// Lane this event was appended on.
    lane: Lane,
    /// Byte size of the JSON-encoded payload (drives the byte cap).
    encoded_bytes: u64,
    /// The event payload, with `seq`/`lane` injected.
    payload: Value,
}

/// Per-lane retained-event/byte/dropped counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LaneCounters {
    /// Retained events in this lane.
    events: u64,
    /// Retained bytes in this lane.
    bytes: u64,
    /// Events evicted from this lane under pressure.
    dropped: u64,
}

/// Bounded in-memory audit ring with lane-priority eviction.
///
/// The ring caps on BOTH event count and byte size; when either ceiling is
/// exceeded it evicts in [`Lane::EVICTION_ORDER`] (sample first, critical last)
/// so critical-lane events survive sample-lane pressure. A rising cross of the
/// pressure threshold is edge-triggered and (in the full port) re-emits a
/// `daemon.audit_buffer_pressure` critical event OUTSIDE the lock.
pub(crate) struct AuditBuffer {
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
    /// Build a ring with the default caps (`50_000` events / 8 MiB) and a fresh
    /// boot epoch id.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::with_caps(DEFAULT_MAX_EVENTS, DEFAULT_MAX_BYTES, None)
    }

    /// Build a ring with explicit caps and an optional fixed boot epoch id.
    #[must_use]
    fn with_caps(max_events: u64, max_bytes: u64, boot_epoch_id: Option<i64>) -> Self {
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
            boot_epoch_id: boot_epoch_id.unwrap_or_else(default_boot_epoch_id),
            pressure_threshold: DEFAULT_PRESSURE_THRESHOLD,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, RingState> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Append `event` on `lane`, returning the assigned sequence number.
    ///
    /// Injects `seq`/`lane` into the payload, enforces the caps (evicting in
    /// lane priority), and on a rising pressure cross re-emits the
    /// `daemon.audit_buffer_pressure` event OUTSIDE the lock.
    fn append(&self, event: Value, lane: Lane) -> u64 {
        let encoded_bytes = encoded_size(&event);
        let mut state = self.lock_state();
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
    pub(crate) fn pull(&self, after_seq: i64, limit: usize) -> Value {
        let limit = limit.max(1);
        let requested_after_seq = after_seq;
        let after_seq = u64::try_from(after_seq).ok();
        let (events, cursor_after, lost_before_seq, buffer, snapshot) = {
            let state = self.lock_state();
            let events: Vec<Value> = state
                .all
                .iter()
                .filter(|event| after_seq.is_none_or(|after_seq| event.seq > after_seq))
                .take(limit)
                .map(|event| event.payload.clone())
                .collect();
            let cursor_after = events
                .last()
                .and_then(|event| event.get("seq"))
                .and_then(Value::as_i64)
                .unwrap_or(requested_after_seq);
            (
                events,
                cursor_after,
                state.lost_before_seq,
                buffer_block(&state),
                snapshot_block(&state, self.boot_epoch_id),
            )
        };
        serde_json::json!({
            "schema": SCHEMA_VERSION,
            "cursor": {
                "after_seq": cursor_after,
                "lost_before_seq": lost_before_seq,
            },
            "buffer": buffer,
            "snapshot": snapshot,
            "events": events,
        })
    }

    /// Buffer + snapshot blocks with no events. Backs `api.audit.snapshot`.
    pub(crate) fn snapshot(&self) -> Value {
        let state = self.lock_state();
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
pub(crate) fn safe_emit(event: Value, lane: Lane) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _ = global_audit_buffer().append(event, lane);
    }));
}

pub(crate) fn global_audit_buffer() -> &'static AuditBuffer {
    static BUFFER: OnceLock<AuditBuffer> = OnceLock::new();
    BUFFER.get_or_init(AuditBuffer::new)
}

fn default_boot_epoch_id() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u128_to_i64_saturating(duration.as_nanos()))
        .unwrap_or_default()
}

fn encoded_size(value: &Value) -> u64 {
    serde_json::to_vec(value).map_or_else(
        |_| usize_to_u64_saturating(format!("{value:?}").len()),
        |bytes| usize_to_u64_saturating(bytes.len()),
    )
}

fn u128_to_i64_saturating(value: u128) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

const fn lane_index(lane: Lane) -> usize {
    match lane {
        Lane::Critical => 0,
        Lane::Normal => 1,
        Lane::Sample => 2,
    }
}

const fn lane_name(lane: Lane) -> &'static str {
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
    (u64_to_f64_lossy(retained_events(state)) / u64_to_f64_lossy(state.max_events))
        .max(u64_to_f64_lossy(retained_bytes(state)) / u64_to_f64_lossy(state.max_bytes))
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    const U32_FACTOR: f64 = 4_294_967_296.0;
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high).mul_add(U32_FACTOR, f64::from(low))
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

#[cfg(test)]
#[path = "../../tests/audit_buffer/mod.rs"]
mod tests;

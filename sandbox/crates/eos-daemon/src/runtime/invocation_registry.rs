//! In-flight invocation registry: invocation id -> task handle, heartbeat,
//! cancel-by-id, and background TTL reaping.
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use tokio::task::AbortHandle;

/// Default TTL before an idle background invocation is reaped (seconds).
pub const DEFAULT_TTL_S: f64 = 300.0;

/// Default reaper sweep interval (seconds).
pub const DEFAULT_REAPER_INTERVAL_S: f64 = 30.0;

/// One tracked daemon-side invocation.
#[derive(Debug)]
pub(crate) struct InFlightInvocation {
    /// Abort handle to the running task (cancel target).
    pub abort: AbortHandle,
    /// Caller that owns this invocation (for per-caller counts).
    pub caller_id: String,
    /// Monotonic seconds of the last heartbeat / registration.
    pub last_seen: f64,
    /// Whether this is a background invocation (only background entries reap).
    pub background: bool,
    /// Set once the reaper has cancelled this entry (idempotent guard).
    pub ttl_reaped: bool,
}

/// Tracks daemon-side tasks by invocation id for cancellation + TTL cleanup.
#[derive(Debug)]
pub struct InFlightRegistry {
    inner: Mutex<HashMap<String, InFlightInvocation>>,
    ttl_s: f64,
    reaper_interval_s: f64,
}

impl InFlightRegistry {
    /// Build a registry with explicit timing values.
    #[must_use]
    pub fn new(ttl_s: f64, reaper_interval_s: f64) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl_s: positive_f64(ttl_s, DEFAULT_TTL_S),
            reaper_interval_s: positive_f64(reaper_interval_s, DEFAULT_REAPER_INTERVAL_S),
        }
    }

    /// Reaper sweep interval (seconds) the daemon's reaper loop sleeps between.
    pub const fn reaper_interval_s(&self) -> f64 {
        self.reaper_interval_s
    }

    // The registry is best-effort daemon control state. If another task panics
    // while holding the mutex, keep cancellation/heartbeat cleanup available
    // instead of panicking future control operations.
    fn lock_state(&self) -> MutexGuard<'_, HashMap<String, InFlightInvocation>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a task under `invocation_id`. Empty ids are ignored.
    pub fn register(
        &self,
        invocation_id: &str,
        abort: AbortHandle,
        caller_id: &str,
        background: bool,
    ) {
        if invocation_id.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        state.insert(
            invocation_id.to_owned(),
            InFlightInvocation {
                abort,
                caller_id: caller_id.to_owned(),
                last_seen: monotonic_seconds(),
                background,
                ttl_reaped: false,
            },
        );
    }

    /// Remove the entry for `invocation_id` (the dispatch `finally` path).
    pub fn deregister(&self, invocation_id: &str) {
        self.lock_state().remove(invocation_id);
    }

    /// Return whether `invocation_id` is still tracked.
    pub fn contains(&self, invocation_id: &str) -> bool {
        self.lock_state().contains_key(invocation_id)
    }

    /// Cancel the task for `invocation_id`; returns whether an entry existed.
    pub fn cancel(&self, invocation_id: &str) -> bool {
        let Some(abort) = ({
            let state = self.lock_state();
            state.get(invocation_id).map(|entry| entry.abort.clone())
        }) else {
            return false;
        };
        abort.abort();
        true
    }

    /// Wait briefly for the dispatch finally path to deregister `invocation_id`.
    pub fn wait_for_cleanup(&self, invocation_id: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.contains(invocation_id) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Touch `last_seen` for every known id; returns how many were touched.
    /// Backs `api.v1.heartbeat`.
    pub fn heartbeat(&self, invocation_ids: &[String]) -> usize {
        let mut state = self.lock_state();
        let now = monotonic_seconds();
        let mut touched = 0;
        for invocation_id in invocation_ids {
            if let Some(entry) = state.get_mut(invocation_id) {
                entry.last_seen = now;
                touched += 1;
            }
        }
        touched
    }

    /// Count live background invocations for `caller_id`. Backs
    /// `api.v1.inflight_count`.
    pub fn count_by_caller(&self, caller_id: &str) -> usize {
        self.lock_state()
            .values()
            .filter(|entry| {
                entry.background
                    && entry.caller_id == caller_id
                    && !entry.ttl_reaped
                    && !entry.abort.is_finished()
            })
            .count()
    }

    /// Cancel every background entry idle past the TTL.
    pub fn ttl_sweep(&self) {
        let mut state = self.lock_state();
        let now = monotonic_seconds();
        for entry in state.values_mut() {
            if entry.background && !entry.ttl_reaped && now - entry.last_seen > self.ttl_s {
                entry.abort.abort();
                entry.ttl_reaped = true;
            }
        }
    }
}

fn positive_f64(value: f64, default: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        default
    }
}

fn monotonic_seconds() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
#[path = "../../tests/unit/invocation_registry/mod.rs"]
mod tests;

//! In-flight invocation registry: invocation id -> task handle, heartbeat,
//! cancel-by-id, and background TTL reaping.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    /// Handle to the running task.
    pub task: InvocationTaskHandle,
    /// Caller that owns this invocation (for per-caller counts).
    pub caller_id: String,
    /// Monotonic seconds of the last heartbeat / registration.
    pub last_seen: f64,
    /// Whether this is a background invocation (only background entries reap).
    pub background: bool,
    /// Set once the reaper has cancelled this entry (idempotent guard).
    pub ttl_reaped: bool,
}

/// Whether a cancel request reached a target and whether it can actually stop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvocationCancelResult {
    Cancelled,
    AlreadyDone,
    RunningUncancellable,
}

#[derive(Debug, Clone)]
pub(crate) enum InvocationTaskHandle {
    Async(AbortHandle),
    Blocking {
        abort: AbortHandle,
        started: Arc<AtomicBool>,
    },
}

impl InvocationTaskHandle {
    fn cancel(&self) -> InvocationCancelResult {
        match self {
            Self::Async(abort) => {
                abort.abort();
                InvocationCancelResult::Cancelled
            }
            Self::Blocking { abort, started } if !started.load(Ordering::SeqCst) => {
                abort.abort();
                InvocationCancelResult::Cancelled
            }
            Self::Blocking { .. } => InvocationCancelResult::RunningUncancellable,
        }
    }

    fn is_finished(&self) -> bool {
        match self {
            Self::Async(abort) | Self::Blocking { abort, .. } => abort.is_finished(),
        }
    }
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
                task: InvocationTaskHandle::Async(abort),
                caller_id: caller_id.to_owned(),
                last_seen: monotonic_seconds(),
                background,
                ttl_reaped: false,
            },
        );
    }

    /// Register a blocking task. Once the task has started, Tokio cannot abort
    /// its blocking closure; cancel reports that distinction instead.
    pub(crate) fn register_blocking(
        &self,
        invocation_id: &str,
        abort: AbortHandle,
        started: Arc<AtomicBool>,
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
                task: InvocationTaskHandle::Blocking { abort, started },
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
        matches!(
            self.cancel_invocation(invocation_id),
            InvocationCancelResult::Cancelled
        )
    }

    pub(crate) fn cancel_invocation(&self, invocation_id: &str) -> InvocationCancelResult {
        let Some(task) = ({
            let state = self.lock_state();
            state.get(invocation_id).map(|entry| entry.task.clone())
        }) else {
            return InvocationCancelResult::AlreadyDone;
        };
        task.cancel()
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
    /// Backs `sandbox.call.heartbeat`.
    pub fn heartbeat(&self, invocation_ids: &[String]) -> usize {
        let mut state = self.lock_state();
        let now = monotonic_seconds();
        let mut touched = 0;
        for invocation_id in invocation_ids {
            if let Some(entry) = state
                .get_mut(invocation_id)
                .filter(|entry| !entry.ttl_reaped)
            {
                entry.last_seen = now;
                touched += 1;
            }
        }
        touched
    }

    /// Count live background invocations for `caller_id`. Backs
    /// `sandbox.call.count`.
    pub fn count_by_caller(&self, caller_id: &str) -> usize {
        self.lock_state()
            .values()
            .filter(|entry| {
                entry.background
                    && entry.caller_id == caller_id
                    && !entry.ttl_reaped
                    && !entry.task.is_finished()
            })
            .count()
    }

    /// Count all tracked invocations, including foreground work.
    pub fn inflight_count(&self) -> usize {
        self.lock_state().len()
    }

    /// Cancel every background entry idle past the TTL.
    pub fn ttl_sweep(&self) {
        let mut state = self.lock_state();
        let now = monotonic_seconds();
        for entry in state.values_mut() {
            if entry.background && !entry.ttl_reaped && now - entry.last_seen > self.ttl_s {
                let _ = entry.task.cancel();
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

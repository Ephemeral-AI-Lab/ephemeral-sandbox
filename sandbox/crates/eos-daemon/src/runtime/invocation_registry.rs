//! In-flight invocation registry + TTL reaper.
//!
//! This is the INVOCATION-keyed registry: id -> task handle, heartbeat ->
//! `last_seen`, cancel-by-id, and the TTL reaper loop. It is DISTINCT from the
//! per-caller isolated-workspace lifecycle state and active command-session records — do not
//! fuse those with this invocation-keyed background-control registry.
//!
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[cfg(target_os = "linux")]
use nix::sys::signal::{killpg, Signal};
#[cfg(target_os = "linux")]
use nix::unistd::Pid;
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
    /// Process group for the namespace runner child, when the invocation owns
    /// one. Cancelling the tokio task alone cannot interrupt a blocking
    /// `wait_with_output`, so cancellation also terminates this group.
    pub process_group_id: Option<i32>,
    /// Set once the reaper has cancelled this entry (idempotent guard).
    pub ttl_reaped: bool,
}

/// Tracks daemon-side tasks by invocation id for cancellation + TTL cleanup.
#[derive(Debug)]
pub struct InFlightRegistry {
    inner: Mutex<RegistryState>,
    ttl_s: f64,
    reaper_interval_s: f64,
}

#[derive(Debug, Default)]
struct RegistryState {
    by_invocation: HashMap<String, InFlightInvocation>,
    ttl_reaped_total: u64,
}

impl InFlightRegistry {
    /// Build a registry with explicit timing values.
    #[must_use]
    pub fn new(ttl_s: f64, reaper_interval_s: f64) -> Self {
        Self {
            inner: Mutex::new(RegistryState::default()),
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
    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
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
        state.by_invocation.insert(
            invocation_id.to_owned(),
            InFlightInvocation {
                abort,
                caller_id: caller_id.to_owned(),
                last_seen: monotonic_seconds(),
                background,
                process_group_id: None,
                ttl_reaped: false,
            },
        );
    }

    /// Attach a process group id to a registered invocation.
    pub fn register_process_group(&self, invocation_id: &str, pgid: i32) {
        if let Some(entry) = self.lock_state().by_invocation.get_mut(invocation_id) {
            entry.process_group_id = Some(pgid);
        }
    }

    /// Clear any process group id attached to a registered invocation.
    pub fn clear_process_group(&self, invocation_id: &str) {
        if let Some(entry) = self.lock_state().by_invocation.get_mut(invocation_id) {
            entry.process_group_id = None;
        }
    }

    /// Remove the entry for `invocation_id` (the dispatch `finally` path).
    pub fn deregister(&self, invocation_id: &str) {
        self.lock_state().by_invocation.remove(invocation_id);
    }

    /// Return whether `invocation_id` is still tracked.
    pub fn contains(&self, invocation_id: &str) -> bool {
        self.lock_state().by_invocation.contains_key(invocation_id)
    }

    /// Cancel the task for `invocation_id`; returns whether an entry existed.
    pub fn cancel(&self, invocation_id: &str) -> bool {
        let Some((abort, process_group_id)) = ({
            let state = self.lock_state();
            state.by_invocation.get(invocation_id).map(|entry| {
                (
                    entry.abort.clone(),
                    entry.process_group_id.filter(|pgid| *pgid > 0),
                )
            })
        }) else {
            return false;
        };
        terminate_process_group(process_group_id);
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
            if let Some(entry) = state.by_invocation.get_mut(invocation_id) {
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
            .by_invocation
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
        let mut reaped = 0;
        for entry in state.by_invocation.values_mut() {
            if entry.background && !entry.ttl_reaped && now - entry.last_seen > self.ttl_s {
                terminate_process_group(entry.process_group_id.filter(|pgid| *pgid > 0));
                entry.abort.abort();
                entry.ttl_reaped = true;
                reaped += 1;
            }
        }
        state.ttl_reaped_total += reaped;
    }

    /// `(active_invocations, ttl_reaped_total)` for diagnostics.
    pub fn metrics(&self) -> (usize, u64) {
        let state = self.lock_state();
        (state.by_invocation.len(), state.ttl_reaped_total)
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

fn terminate_process_group(process_group_id: Option<i32>) {
    let Some(pgid) = process_group_id else {
        return;
    };
    #[cfg(target_os = "linux")]
    {
        let pid = Pid::from_raw(pgid);
        if killpg(pid, Signal::SIGTERM).is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = killpg(pid, Signal::SIGKILL);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pgid;
    }
}

#[cfg(test)]
#[path = "../../tests/invocation_registry/mod.rs"]
mod tests;

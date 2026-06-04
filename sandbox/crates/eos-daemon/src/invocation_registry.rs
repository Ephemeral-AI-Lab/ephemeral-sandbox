//! In-flight invocation registry + TTL reaper.
//!
//! This is the INVOCATION-keyed registry: id -> task handle, heartbeat ->
//! `last_seen`, cancel-by-id, and the TTL reaper loop. It is DISTINCT from the
//! per-agent isolated-workspace lifecycle state and active command-session records — do not
//! fuse those with this invocation-keyed background-control registry.
//!
//! # Source divergence (noted, not silently resolved)
//!
//! * The task names `EOS_BACKGROUND_HEARTBEAT_INTERVAL_S`; the live Python uses
//!   [`ENV_TTL_S`] (`EOS_INFLIGHT_TTL_S`, default 300s) and
//!   [`ENV_REAPER_INTERVAL_S`] (`EOS_INFLIGHT_REAPER_INTERVAL_S`, default 30s).
//!   We reproduce the source env vars; the heartbeat-interval naming is a
//!   port-time reconciliation.
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

/// Env override for the TTL.
pub const ENV_TTL_S: &str = "EOS_INFLIGHT_TTL_S";

/// Env override for the reaper interval.
pub const ENV_REAPER_INTERVAL_S: &str = "EOS_INFLIGHT_REAPER_INTERVAL_S";

/// One tracked daemon-side invocation.
#[derive(Debug)]
pub struct InFlightInvocation {
    /// The invocation id (registry key).
    pub invocation_id: String,
    /// Abort handle to the running task (cancel target).
    pub abort: AbortHandle,
    /// Agent that owns this invocation (for per-agent counts).
    pub agent_id: String,
    /// The op name (for diagnostics).
    pub op: String,
    /// Monotonic seconds of the last heartbeat / registration.
    pub last_seen: f64,
    /// Whether this is a background invocation (only background entries reap).
    pub background: bool,
    /// Concurrently active runtime calls; retained for diagnostics/parity with
    /// active-call bookkeeping, but stale background cancellation does not wait
    /// for the call to go idle.
    pub active_calls: u32,
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

    /// Build a registry, sourcing TTL / reaper interval from env (falling back
    /// to the defaults).
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(
            env_positive_f64(ENV_TTL_S, DEFAULT_TTL_S),
            env_positive_f64(ENV_REAPER_INTERVAL_S, DEFAULT_REAPER_INTERVAL_S),
        )
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
        agent_id: &str,
        op: &str,
        background: bool,
    ) {
        if invocation_id.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        state.by_invocation.insert(
            invocation_id.to_owned(),
            InFlightInvocation {
                invocation_id: invocation_id.to_owned(),
                abort,
                agent_id: agent_id.to_owned(),
                op: op.to_owned(),
                last_seen: monotonic_seconds(),
                background,
                active_calls: 0,
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

    /// Count live background invocations for `agent_id`. Backs
    /// `api.v1.inflight_count`.
    pub fn count_by_agent(&self, agent_id: &str) -> usize {
        self.lock_state()
            .by_invocation
            .values()
            .filter(|entry| {
                entry.background
                    && entry.agent_id == agent_id
                    && !entry.ttl_reaped
                    && !entry.abort.is_finished()
            })
            .count()
    }

    /// Acquire an [`ActiveCallGuard`]: bumps `active_calls` for diagnostics.
    /// The guard decrements on drop.
    pub fn enter_call<'r>(&'r self, invocation_id: &str) -> ActiveCallGuard<'r> {
        if let Some(entry) = self.lock_state().by_invocation.get_mut(invocation_id) {
            entry.active_calls = entry.active_calls.saturating_add(1);
        }
        ActiveCallGuard {
            registry: self,
            invocation_id: invocation_id.to_owned(),
        }
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

/// RAII guard counting one active runtime call against an invocation.
///
/// Holding it keeps `active_calls > 0`; dropping it decrements the counter.
#[derive(Debug)]
#[must_use = "dropping this guard decrements the active-call count; bind it for the call's duration"]
pub struct ActiveCallGuard<'r> {
    registry: &'r InFlightRegistry,
    invocation_id: String,
}

impl Drop for ActiveCallGuard<'_> {
    fn drop(&mut self) {
        if let Some(entry) = self
            .registry
            .lock_state()
            .by_invocation
            .get_mut(&self.invocation_id)
        {
            entry.active_calls = entry.active_calls.saturating_sub(1);
        }
    }
}

/// Read a positive `f64` env var, falling back to `default` on absent/invalid/<=0.
fn env_positive_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).map_or(default, |raw| {
        raw.trim()
            .parse::<f64>()
            .map_or(default, |value| positive_f64(value, default))
    })
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
mod tests {
    use std::future;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use super::InFlightRegistry;
    use tokio::task::JoinHandle;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[tokio::test]
    async fn cancel_heartbeat_and_count_track_background_task() -> TestResult {
        let registry = InFlightRegistry::new(300.0, 30.0);
        let task = tokio::spawn(future::pending::<()>());
        registry.register(
            "bg-1",
            task.abort_handle(),
            "agent-a",
            "api.v1.exec_command",
            true,
        );

        assert_eq!(registry.count_by_agent("agent-a"), 1);
        assert_eq!(
            registry.heartbeat(&["bg-1".to_owned(), "missing".to_owned()]),
            1
        );
        assert!(registry.cancel("bg-1"));
        assert_task_cancelled(task).await?;
        assert_eq!(registry.count_by_agent("agent-a"), 0);

        registry.deregister("bg-1");
        assert_eq!(registry.metrics(), (0, 0));
        Ok(())
    }

    #[tokio::test]
    async fn control_paths_recover_poisoned_registry_lock() -> TestResult {
        let registry = Arc::new(InFlightRegistry::new(300.0, 30.0));
        let poisoned = registry.clone();
        let poison_result = thread::spawn(move || {
            let _guard = match poisoned.inner.lock() {
                Ok(guard) => guard,
                Err(error) => error.into_inner(),
            };
            std::panic::resume_unwind(Box::new("poison in-flight registry"));
        })
        .join();
        if poison_result.is_ok() {
            return Err("poison helper thread completed without unwinding".into());
        }

        let task = tokio::spawn(future::pending::<()>());
        registry.register(
            "bg-poisoned",
            task.abort_handle(),
            "agent-a",
            "api.v1.exec_command",
            true,
        );

        assert_eq!(registry.count_by_agent("agent-a"), 1);
        assert_eq!(registry.heartbeat(&["bg-poisoned".to_owned()]), 1);
        {
            let _guard = registry.enter_call("bg-poisoned");
            registry.ttl_sweep();
        }
        assert!(registry.cancel("bg-poisoned"));
        assert_task_cancelled(task).await?;
        registry.deregister("bg-poisoned");
        assert_eq!(registry.metrics(), (0, 0));
        Ok(())
    }

    #[tokio::test]
    async fn ttl_sweep_reaps_active_background_task() -> TestResult {
        let registry = InFlightRegistry::new(0.001, 30.0);
        let task = tokio::spawn(future::pending::<()>());
        registry.register(
            "bg-ttl",
            task.abort_handle(),
            "agent-a",
            "api.v1.exec_command",
            true,
        );

        {
            let _active = registry.enter_call("bg-ttl");
            thread::sleep(Duration::from_millis(3));
            registry.ttl_sweep();
            assert_eq!(registry.metrics(), (1, 1));
            assert_eq!(registry.count_by_agent("agent-a"), 0);
        }

        assert_task_cancelled(task).await?;
        assert_eq!(registry.count_by_agent("agent-a"), 0);
        Ok(())
    }

    async fn assert_task_cancelled(task: JoinHandle<()>) -> TestResult {
        match task.await {
            Ok(()) => Err("expected task cancellation, but task completed".into()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(format!("expected task cancellation, got {error}").into()),
        }
    }
}

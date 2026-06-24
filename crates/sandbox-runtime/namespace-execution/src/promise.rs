use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::NamespaceExecutionError;

/// Write-once completion cell: the single internal "done?" truth, backed by a
/// `Mutex` + `Condvar`. `wait` takes the value (single-consumer); `resolved`
/// peeks a clone without consuming, so the registry-retained handle can serve
/// repeated terminal reads.
pub struct CompletionPromise<T> {
    slot: Mutex<Option<Result<T, NamespaceExecutionError>>>,
    ready: Condvar,
}

impl<T> Default for CompletionPromise<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> CompletionPromise<T> {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    /// Pending → resolved, then `notify_all`. Returns `false` if already resolved.
    pub fn resolve(&self, outcome: Result<T, NamespaceExecutionError>) -> bool {
        let mut slot = self.slot.lock().expect("completion promise mutex poisoned");
        if slot.is_none() {
            *slot = Some(outcome);
            self.ready.notify_all();
            true
        } else {
            false
        }
    }

    pub fn is_resolved(&self) -> bool {
        let slot = self.slot.lock().expect("completion promise mutex poisoned");
        slot.is_some()
    }

    /// Block until resolved, then take the value (single-consumer).
    pub fn wait(&self) -> Result<T, NamespaceExecutionError> {
        let mut slot = self.slot.lock().expect("completion promise mutex poisoned");
        while slot.is_none() {
            slot = self
                .ready
                .wait(slot)
                .expect("completion promise mutex poisoned");
        }
        slot.take()
            .expect("wait loop exits only once the slot is resolved")
    }

    /// Block up to `timeout` for resolution. `true` once resolved, `false` on
    /// timeout. Does not consume the value — it stays for `wait`/`resolved`.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let mut slot = self.slot.lock().expect("completion promise mutex poisoned");
        let deadline = Instant::now() + timeout;
        while slot.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, outcome) = self
                .ready
                .wait_timeout(slot, remaining)
                .expect("completion promise mutex poisoned");
            slot = next;
            if outcome.timed_out() && slot.is_none() {
                return false;
            }
        }
        true
    }

    /// Non-consuming snapshot of the resolved outcome, or `None` while pending.
    /// The slot is never taken, so repeated terminal reads each get a clone.
    pub fn resolved(&self) -> Option<Result<T, NamespaceExecutionError>>
    where
        T: Clone,
    {
        let slot = self.slot.lock().expect("completion promise mutex poisoned");
        slot.clone()
    }
}

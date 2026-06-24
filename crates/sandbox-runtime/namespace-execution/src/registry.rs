use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::NamespaceExecutionError;
use crate::id::NamespaceExecutionId;
use crate::status::NamespaceExecutionTerminalStatus;

/// Executions keyed by `NamespaceExecutionId`, generic over the caller value `V`
/// the registry retains for the live + terminal phases (the command handle in
/// Phase 3; `()` for mount). Shared as `Arc<ExecutionRegistry<V>>`; the watcher
/// thread calls `complete`, which touches only the terminal projection — never
/// `V` — so an `attach` racing a `complete` is benign.
pub struct ExecutionRegistry<V> {
    inner: Mutex<RegistryState<V>>,
    max_active: usize,
}

struct RegistryState<V> {
    entries: HashMap<NamespaceExecutionId, Entry<V>>,
    active: usize,
}

struct Entry<V> {
    value: Option<V>,
    terminal: bool,
    status: Option<NamespaceExecutionTerminalStatus>,
    exit: Option<i64>,
}

impl<V> Entry<V> {
    const fn reserved() -> Self {
        Self {
            value: None,
            terminal: false,
            status: None,
            exit: None,
        }
    }
}

impl<V> ExecutionRegistry<V> {
    #[must_use]
    pub fn new(max_active: usize) -> Self {
        Self {
            inner: Mutex::new(RegistryState {
                entries: HashMap::new(),
                active: 0,
            }),
            max_active,
        }
    }

    #[must_use]
    pub fn max_active(&self) -> usize {
        self.max_active
    }

    /// Atomically reserve a live slot keyed by `id`; `Err(Admission)` if full.
    /// The capacity check and the insert happen under one lock, so concurrent
    /// `run_*` calls cannot both admit the last slot. The entry's `value` is
    /// filled later by `attach`.
    pub fn try_reserve(&self, id: &NamespaceExecutionId) -> Result<(), NamespaceExecutionError> {
        let mut state = self.lock();
        if state.active >= self.max_active {
            return Err(NamespaceExecutionError::Admission {
                max_active: self.max_active,
            });
        }
        state.entries.insert(id.clone(), Entry::reserved());
        state.active += 1;
        Ok(())
    }

    /// Attach the caller value to a reserved (or already terminal) slot. Filling
    /// `value` regardless of terminal state is what makes the attach/complete
    /// race benign: the watcher may `complete` before the caller `attach`es.
    pub fn attach(&self, id: &NamespaceExecutionId, value: V) {
        if let Some(entry) = self.lock().entries.get_mut(id) {
            entry.value = Some(value);
        }
    }

    /// Release a reservation on spawn failure.
    pub fn abort(&self, id: &NamespaceExecutionId) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.remove(id) {
            if !entry.terminal {
                state.active = state.active.saturating_sub(1);
            }
        }
    }

    /// Mark an execution terminal and release its admission slot. Idempotent and
    /// generic: it never reads or writes `V`, so the retained value stays
    /// readable through `with_value`/`live_values` for both phases.
    pub fn complete(
        &self,
        id: &NamespaceExecutionId,
        status: NamespaceExecutionTerminalStatus,
        exit: Option<i64>,
    ) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(id) {
            if !entry.terminal {
                entry.terminal = true;
                entry.status = Some(status);
                entry.exit = exit;
                state.active = state.active.saturating_sub(1);
            }
        }
    }

    /// Read the retained value under the registry lock, if present.
    pub fn with_value<R>(&self, id: &NamespaceExecutionId, f: impl FnOnce(&V) -> R) -> Option<R> {
        self.lock()
            .entries
            .get(id)
            .and_then(|entry| entry.value.as_ref())
            .map(f)
    }

    #[must_use]
    pub fn is_live(&self, id: &NamespaceExecutionId) -> bool {
        self.lock()
            .entries
            .get(id)
            .is_some_and(|entry| !entry.terminal)
    }

    #[must_use]
    pub fn is_completed(&self, id: &NamespaceExecutionId) -> bool {
        self.lock()
            .entries
            .get(id)
            .is_some_and(|entry| entry.terminal)
    }

    /// Project every live execution's retained value through `f`, collecting the
    /// `Some` results. The Phase-5 hook for "live interactive executions in a
    /// workspace" (pgid/cancel) without a second per-session map.
    pub fn live_values<R>(&self, f: impl Fn(&V) -> Option<R>) -> Vec<R> {
        self.lock()
            .entries
            .values()
            .filter(|entry| !entry.terminal)
            .filter_map(|entry| entry.value.as_ref())
            .filter_map(f)
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryState<V>> {
        self.inner
            .lock()
            .expect("execution registry mutex poisoned")
    }
}

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::NamespaceExecutionError;
use crate::types::NamespaceExecutionId;
use crate::shell::NamespaceExecutionTerminalStatus;

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
}

impl<V> Entry<V> {
    const fn reserved() -> Self {
        Self {
            value: None,
            terminal: false,
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

    pub fn attach(&self, id: &NamespaceExecutionId, value: V) {
        if let Some(entry) = self.lock().entries.get_mut(id) {
            entry.value = Some(value);
        }
    }

    pub fn abort(&self, id: &NamespaceExecutionId) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.remove(id) {
            if !entry.terminal {
                state.active = state.active.saturating_sub(1);
            }
        }
    }

    pub fn complete(
        &self,
        id: &NamespaceExecutionId,
        _status: NamespaceExecutionTerminalStatus,
        _exit: Option<i64>,
    ) {
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(id) {
            if !entry.terminal {
                entry.terminal = true;
                state.active = state.active.saturating_sub(1);
            }
        }
    }

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

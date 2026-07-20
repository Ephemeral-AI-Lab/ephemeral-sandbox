use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::error::NamespaceExecutionError;
use crate::shell::NamespaceExecutionTerminalStatus;
use crate::types::NamespaceExecutionId;

/// Bounded execution table. Marking an entry terminal evicts the oldest
/// terminal entry beyond `max_terminal`, dropping its value and releasing
/// whatever the value's own `Drop` owns. A drain against an evicted id observes
/// a missing entry.
pub struct ExecutionRegistry<V> {
    inner: Mutex<RegistryState<V>>,
    max_active: usize,
}

struct RegistryState<V> {
    entries: HashMap<NamespaceExecutionId, Entry<V>>,
    active: usize,
    terminal_order: VecDeque<NamespaceExecutionId>,
    max_terminal: usize,
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
    pub fn new(max_active: usize, max_terminal: usize) -> Self {
        Self {
            inner: Mutex::new(RegistryState {
                entries: HashMap::with_capacity(1),
                active: 0,
                terminal_order: VecDeque::with_capacity(1),
                max_terminal,
            }),
            max_active,
        }
    }

    /// Override the terminal-entry retention cap (initialized from
    /// [`crate::ExecutionCaps::max_terminal_entries`]). An over-cap backlog is
    /// trimmed on the next terminal transition, not immediately.
    pub fn set_terminal_retention(&self, max_terminal: usize) {
        self.lock().max_terminal = max_terminal;
    }

    pub fn try_reserve(&self, id: &NamespaceExecutionId) -> Result<(), NamespaceExecutionError> {
        let mut state = self.lock();
        if state.entries.contains_key(id) {
            return Err(NamespaceExecutionError::Duplicate {
                execution_id: id.0.clone(),
            });
        }
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
        if state.entries.get(id).is_some_and(|entry| entry.terminal) {
            return;
        }
        if state.entries.remove(id).is_some() {
            state.active = state.active.saturating_sub(1);
        }
    }

    pub fn complete(
        &self,
        id: &NamespaceExecutionId,
        _status: NamespaceExecutionTerminalStatus,
        _exit: Option<i64>,
    ) {
        let mut evicted = Vec::new();
        {
            let mut state = self.lock();
            if let Some(entry) = state.entries.get_mut(id) {
                if !entry.terminal {
                    entry.terminal = true;
                    state.active = state.active.saturating_sub(1);
                    state.terminal_order.push_back(id.clone());
                    while state.terminal_order.len() > state.max_terminal {
                        let Some(oldest) = state.terminal_order.pop_front() else {
                            break;
                        };
                        if let Some(entry) = state.entries.remove(&oldest) {
                            evicted.push(entry);
                        }
                    }
                }
            }
        }
        drop(evicted);
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

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.lock().active
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

    pub fn remove_terminal_values(&self, mut predicate: impl FnMut(&V) -> bool) -> usize {
        let removed = {
            let mut state = self.lock();
            let mut removed = Vec::new();
            let terminal_count = state.terminal_order.len();
            for _ in 0..terminal_count {
                let Some(id) = state.terminal_order.pop_front() else {
                    break;
                };
                let should_remove = state
                    .entries
                    .get(&id)
                    .filter(|entry| entry.terminal)
                    .and_then(|entry| entry.value.as_ref())
                    .is_some_and(&mut predicate);
                if should_remove {
                    if let Some(entry) = state.entries.remove(&id) {
                        removed.push(entry);
                    }
                } else {
                    state.terminal_order.push_back(id);
                }
            }
            if !removed.is_empty() {
                state.entries.shrink_to_fit();
                state.terminal_order.shrink_to_fit();
            }
            removed
        };
        let count = removed.len();
        drop(removed);
        count
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryState<V>> {
        self.inner
            .lock()
            .expect("execution registry mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(number: usize) -> NamespaceExecutionId {
        NamespaceExecutionId(format!("namespace_execution_{number}"))
    }

    #[test]
    fn bulk_terminal_release_compacts_backing_storage() {
        let registry = ExecutionRegistry::new(64, 64);
        for number in 0..32 {
            let execution_id = id(number);
            registry.try_reserve(&execution_id).expect("reserve slot");
            registry.attach(&execution_id, "workspace-a".to_owned());
            registry.complete(&execution_id, NamespaceExecutionTerminalStatus::Ok, Some(0));
        }
        let (entries_high_water, order_high_water) = {
            let state = registry.lock();
            (state.entries.capacity(), state.terminal_order.capacity())
        };
        assert!(entries_high_water >= 32);
        assert!(order_high_water >= 32);

        assert_eq!(
            registry.remove_terminal_values(|value| value == "workspace-a"),
            32
        );

        let state = registry.lock();
        assert!(state.entries.is_empty());
        assert!(state.terminal_order.is_empty());
        assert!(state.entries.capacity() < entries_high_water);
        assert!(state.terminal_order.capacity() < order_high_water);
    }
}

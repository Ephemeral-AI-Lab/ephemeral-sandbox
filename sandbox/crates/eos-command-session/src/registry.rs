use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::session::CommandSession;
use crate::{CollectCompleted, CollectCompletedResponse, CommandResponse};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandSessionCompletion {
    pub command_session_id: String,
    pub caller_id: String,
    pub command: String,
    pub result: CommandResponse,
    pub notification_result: CommandResponse,
}

/// Hard cap on parked (completed-but-uncollected) sessions. A caller that never
/// calls `collect_completed` would otherwise grow this map without bound; on
/// overflow the oldest uncollected completion is dropped. Silently losing a stale
/// completion is acceptable at a backstop this high — normal callers collect
/// promptly and never approach it.
const MAX_COMPLETED_ENTRIES: usize = 1024;

struct CompletedEntry {
    seq: u64,
    completion: CommandSessionCompletion,
}

#[derive(Default)]
pub(crate) struct CommandSessionRegistry {
    sessions: Mutex<HashMap<String, Arc<CommandSession>>>,
    completed: Mutex<HashMap<String, CompletedEntry>>,
    counter: AtomicU64,
    completed_seq: AtomicU64,
}

impl CommandSessionRegistry {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
            completed_seq: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub(crate) fn next_id(&self) -> String {
        format!("cmd_{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn insert(&self, session: Arc<CommandSession>) {
        lock(&self.sessions).insert(session.id().to_owned(), session);
    }

    #[must_use]
    pub(crate) fn get(&self, id: &str) -> Option<Arc<CommandSession>> {
        lock(&self.sessions).get(id).cloned()
    }

    pub(crate) fn remove(&self, id: &str) -> Option<Arc<CommandSession>> {
        lock(&self.sessions).remove(id)
    }

    #[must_use]
    pub(crate) fn count_by_caller(&self, caller_id: Option<&str>) -> usize {
        lock(&self.sessions)
            .values()
            .filter(|session| caller_id.is_none_or(|caller_id| session.caller_id() == caller_id))
            .count()
    }

    #[must_use]
    pub(crate) fn live(&self) -> Vec<Arc<CommandSession>> {
        lock(&self.sessions).values().cloned().collect()
    }

    pub(crate) fn push_completed(&self, completion: CommandSessionCompletion) {
        let seq = self.completed_seq.fetch_add(1, Ordering::Relaxed);
        let mut completed = lock(&self.completed);
        completed.insert(
            completion.command_session_id.clone(),
            CompletedEntry { seq, completion },
        );
        // Bound memory: drop the oldest uncollected completion(s) past the cap.
        while completed.len() > MAX_COMPLETED_ENTRIES {
            let Some(oldest) = completed
                .iter()
                .min_by_key(|(_, entry)| entry.seq)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            completed.remove(&oldest);
        }
    }

    pub(crate) fn take_completed_result(&self, id: &str) -> Option<CommandResponse> {
        lock(&self.completed)
            .remove(id)
            .map(|entry| entry.completion.result)
    }

    #[must_use]
    pub(crate) fn completed_result(&self, id: &str) -> Option<CommandResponse> {
        lock(&self.completed)
            .get(id)
            .map(|entry| entry.completion.result.clone())
    }

    #[must_use]
    pub(crate) fn collect_completed(&self, request: &CollectCompleted) -> CollectCompletedResponse {
        let wanted: Option<HashSet<String>> = request
            .command_session_ids
            .as_ref()
            .map(|ids| ids.iter().cloned().collect());
        let caller_id = request.caller_id.as_deref();
        let mut completed = lock(&self.completed);
        let matched: Vec<String> = completed
            .iter()
            .filter(|(id, entry)| {
                let id_matches = wanted.as_ref().is_none_or(|ids| ids.contains(*id));
                let caller_matches =
                    caller_id.is_none_or(|caller_id| entry.completion.caller_id == caller_id);
                id_matches && caller_matches
            })
            .map(|(id, _)| id.clone())
            .collect();
        let completions = matched
            .iter()
            .filter_map(|id| completed.remove(id))
            .map(|entry| {
                let mut completion = entry.completion;
                completion.result = completion.notification_result.clone();
                completion
            })
            .collect();
        CollectCompletedResponse {
            success: true,
            completions,
        }
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_completion(id: &str) -> CommandSessionCompletion {
        let result = CommandResponse::error("");
        CommandSessionCompletion {
            command_session_id: id.to_owned(),
            caller_id: "caller".to_owned(),
            command: "cmd".to_owned(),
            result: result.clone(),
            notification_result: result,
        }
    }

    #[test]
    fn push_completed_evicts_oldest_beyond_cap() {
        let registry = CommandSessionRegistry::new();
        let overflow = 5;
        for index in 0..(MAX_COMPLETED_ENTRIES + overflow) {
            registry.push_completed(sample_completion(&format!("cmd_{index}")));
        }
        // The map is bounded at the cap.
        assert_eq!(lock(&registry.completed).len(), MAX_COMPLETED_ENTRIES);
        // The oldest `overflow` completions were evicted.
        for index in 0..overflow {
            assert!(registry
                .take_completed_result(&format!("cmd_{index}"))
                .is_none());
        }
        // A recent completion is retained and still collectable.
        let newest = format!("cmd_{}", MAX_COMPLETED_ENTRIES + overflow - 1);
        assert!(registry.take_completed_result(&newest).is_some());
    }
}

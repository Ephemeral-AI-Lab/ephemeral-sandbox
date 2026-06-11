// Non-Linux keeps this compiled for scaffold unit tests and a uniform module tree.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use std::path::PathBuf;

use eos_command_session::session::CommandSession;
use eos_command_session::{
    CollectCompleted, CollectCompletedResponse, CommandResponse, CommandSessionCompletion,
};
use eos_ephemeral_workspace::EphemeralWorkspace;
use eos_layerstack::service::Snapshot;

use crate::CommandBinding;

pub(crate) struct EphemeralRun {
    pub(crate) session: CommandSession,
    pub(crate) root: PathBuf,
    pub(crate) snapshot: Snapshot,
    pub(crate) workspace: EphemeralWorkspace,
}

pub(crate) struct IsolatedRun {
    pub(crate) session: CommandSession,
    pub(crate) binding: CommandBinding,
}

pub(crate) enum ActiveCommand {
    Ephemeral(EphemeralRun),
    Isolated(IsolatedRun),
}

impl ActiveCommand {
    pub(crate) fn session(&self) -> &CommandSession {
        match self {
            Self::Ephemeral(run) => &run.session,
            Self::Isolated(run) => &run.session,
        }
    }
}

const MAX_COMPLETED_ENTRIES: usize = 1024;

struct CompletedEntry {
    seq: u64,
    completion: CommandSessionCompletion,
}

#[derive(Default)]
pub(crate) struct CommandRegistry {
    runs: Mutex<HashMap<String, HashMap<String, Arc<ActiveCommand>>>>,
    completed: Mutex<HashMap<String, CompletedEntry>>,
    counter: AtomicU64,
    completed_seq: AtomicU64,
}

impl CommandRegistry {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
            completed_seq: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub(crate) fn next_id(&self) -> String {
        format!("cmd_{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn insert(&self, run: Arc<ActiveCommand>) {
        let caller_id = run.session().caller_id().to_owned();
        let session_id = run.session().id().to_owned();
        lock(&self.runs)
            .entry(caller_id)
            .or_default()
            .insert(session_id, run);
    }

    #[must_use]
    pub(crate) fn get(&self, id: &str) -> Option<Arc<ActiveCommand>> {
        lock(&self.runs)
            .values()
            .find_map(|runs| runs.get(id).cloned())
    }

    pub(crate) fn remove(&self, id: &str) -> Option<Arc<ActiveCommand>> {
        let mut runs = lock(&self.runs);
        let caller = runs
            .iter()
            .find(|(_, caller_runs)| caller_runs.contains_key(id))
            .map(|(caller, _)| caller.clone())?;
        let run = runs.get_mut(&caller)?.remove(id);
        if runs.get(&caller).is_some_and(HashMap::is_empty) {
            runs.remove(&caller);
        }
        run
    }

    #[must_use]
    pub(crate) fn count_by_caller(&self, caller_id: Option<&str>) -> usize {
        let runs = lock(&self.runs);
        match caller_id {
            Some(caller) => runs.get(caller).map_or(0, HashMap::len),
            None => runs.values().map(HashMap::len).sum(),
        }
    }

    #[must_use]
    pub(crate) fn live(&self) -> Vec<Arc<ActiveCommand>> {
        lock(&self.runs)
            .values()
            .flat_map(|runs| runs.values().cloned())
            .collect()
    }

    #[must_use]
    pub(crate) fn caller_sessions(&self, caller_id: &str) -> Vec<Arc<ActiveCommand>> {
        lock(&self.runs)
            .get(caller_id)
            .map(|runs| runs.values().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn push_completed(&self, completion: CommandSessionCompletion) {
        let seq = self.completed_seq.fetch_add(1, Ordering::Relaxed);
        let mut completed = lock(&self.completed);
        completed.insert(
            completion.command_session_id.clone(),
            CompletedEntry { seq, completion },
        );
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
            .map(|entry| entry.completion)
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
#[path = "../tests/unit/registry.rs"]
mod tests;

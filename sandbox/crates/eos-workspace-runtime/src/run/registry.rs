// The caller-keyed registry is the Linux PTY/overlay orchestration. On non-Linux
// the daemon serves command-session ops as stubs, so most of the registry is dead
// there — it stays compiled for the scaffold unit tests and a uniform module tree.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::command_session::session::CommandSession;
use crate::command_session::{
    CollectCompleted, CollectCompletedResponse, CommandResponse, CommandSessionCompletion,
};
use crate::ephemeral::EphemeralWorkspace;

use super::command_handle::CommandHandle;

/// One ephemeral workspace run: exactly **one** command session paired with the
/// fresh overlay it owns (snapshot lease + run dirs). The run owns the overlay
/// state directly — there is no policy indirection — so completion publishes the
/// captured upperdir and cancellation discards it, structurally.
pub(crate) struct EphemeralRun {
    pub(crate) session: CommandSession,
    pub(crate) workspace: EphemeralWorkspace,
}

/// One command session running inside a caller's isolated workspace. It carries
/// the per-session [`CommandHandle`] (namespace fds, scratch dirs, lease/manifest
/// coordinates) needed to finalize the session for AUDIT (never published); the
/// namespace + lease themselves are owned by the isolated-session subsystem and
/// torn down on `exit`.
pub(crate) struct IsolatedRun {
    pub(crate) session: CommandSession,
    pub(crate) handle: CommandHandle,
}

/// A single workspace run: an ephemeral overlay run or one session of the
/// caller's isolated run. The variant carries the per-kind state the daemon needs
/// to publish (ephemeral complete), discard (cancel), or audit (isolated).
pub(crate) enum WorkspaceRun {
    Ephemeral(EphemeralRun),
    Isolated(IsolatedRun),
}

impl WorkspaceRun {
    pub(crate) fn session(&self) -> &CommandSession {
        match self {
            Self::Ephemeral(run) => &run.session,
            Self::Isolated(run) => &run.session,
        }
    }
}

/// A caller's command-session runs, keyed by command-session id. A caller holds
/// many ephemeral workspace runs (each one session) **or** its one isolated run's
/// many sessions; the ephemeral-vs-isolated XOR is enforced by the isolated
/// enter/exit gate, not here. Each [`WorkspaceRun`] is self-describing
/// (`Ephemeral`/`Isolated`), so cancel/settle dispatch on the run, and the
/// registry needs no per-caller variant tag — one map holds whichever kind the
/// caller currently runs.
#[derive(Default)]
struct CallerRuns(HashMap<String, Arc<WorkspaceRun>>);

impl CallerRuns {
    fn sessions(&self) -> Vec<Arc<WorkspaceRun>> {
        self.0.values().cloned().collect()
    }

    fn get(&self, session_id: &str) -> Option<Arc<WorkspaceRun>> {
        self.0.get(session_id).cloned()
    }

    fn count(&self) -> usize {
        self.0.len()
    }

    fn insert(&mut self, session_id: String, run: Arc<WorkspaceRun>) {
        self.0.insert(session_id, run);
    }

    /// Remove `session_id`, returning the removed run and whether this caller is
    /// now empty (so the registry can drop the caller entry).
    fn take(&mut self, session_id: &str) -> (Option<Arc<WorkspaceRun>>, bool) {
        let removed = self.0.remove(session_id);
        (removed, self.0.is_empty())
    }
}

/// Hard cap on parked (completed-but-uncollected) sessions across **all** callers
/// — the completion queue is one daemon-global map, not per caller. A caller that
/// never calls `collect_completed` would otherwise grow it without bound; on
/// overflow the oldest uncollected completion is dropped (silently — the daemon
/// has no log surface). The cap is high enough that normal callers, which the
/// heartbeat drains every tick, never approach it; the accepted residual is that
/// one caller bursting past the cap could evict another caller's stale completion.
const MAX_COMPLETED_ENTRIES: usize = 1024;

struct CompletedEntry {
    seq: u64,
    completion: CommandSessionCompletion,
}

/// Single caller-keyed command-session authority. Each caller maps to its
/// [`CallerRuns`] (many ephemeral workspace runs or the one isolated run's
/// sessions). Session-targeted ops resolve by scanning runs for the session id
/// (caller count is small).
#[derive(Default)]
pub(crate) struct WorkspaceRunRegistry {
    runs: Mutex<HashMap<String, CallerRuns>>,
    completed: Mutex<HashMap<String, CompletedEntry>>,
    counter: AtomicU64,
    completed_seq: AtomicU64,
}

impl WorkspaceRunRegistry {
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

    /// File a started run under its caller, keyed by command-session id. Total by
    /// construction — a freshly spawned run is never silently dropped.
    pub(crate) fn insert(&self, run: Arc<WorkspaceRun>) {
        let caller_id = run.session().caller_id().to_owned();
        let session_id = run.session().id().to_owned();
        lock(&self.runs)
            .entry(caller_id)
            .or_default()
            .insert(session_id, run);
    }

    #[must_use]
    pub(crate) fn get(&self, id: &str) -> Option<Arc<WorkspaceRun>> {
        lock(&self.runs).values().find_map(|run| run.get(id))
    }

    pub(crate) fn remove(&self, id: &str) -> Option<Arc<WorkspaceRun>> {
        let mut runs = lock(&self.runs);
        let caller = runs
            .iter()
            .find(|(_, run)| run.get(id).is_some())
            .map(|(caller, _)| caller.clone())?;
        let (run, now_empty) = runs
            .get_mut(&caller)
            .map(|run| run.take(id))
            .unwrap_or((None, false));
        if now_empty {
            runs.remove(&caller);
        }
        run
    }

    #[must_use]
    pub(crate) fn count_by_caller(&self, caller_id: Option<&str>) -> usize {
        let runs = lock(&self.runs);
        match caller_id {
            Some(caller) => runs.get(caller).map_or(0, CallerRuns::count),
            None => runs.values().map(CallerRuns::count).sum(),
        }
    }

    #[must_use]
    pub(crate) fn live(&self) -> Vec<Arc<WorkspaceRun>> {
        lock(&self.runs)
            .values()
            .flat_map(CallerRuns::sessions)
            .collect()
    }

    /// All live runs owned by `caller_id` (drives per-caller cleanup).
    #[must_use]
    pub(crate) fn caller_sessions(&self, caller_id: &str) -> Vec<Arc<WorkspaceRun>> {
        lock(&self.runs)
            .get(caller_id)
            .map(CallerRuns::sessions)
            .unwrap_or_default()
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
#[path = "../../tests/run/registry_unit.rs"]
mod tests;

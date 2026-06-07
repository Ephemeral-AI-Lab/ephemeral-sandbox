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

/// Which workspace a starting command session belongs to. The daemon picks the
/// kind from the caller's current mode; the registry uses it to place the session
/// into a fresh ephemeral workspace run or the caller's isolated run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceRunKind {
    Ephemeral,
    Isolated,
}

/// One ephemeral workspace run: **exactly one** command session. Each ephemeral
/// `exec_command` gets its own workspace (its snapshot lease + run dirs live in
/// the session's policy today), so the workspace and its session are 1:1 and
/// co-terminal.
struct EphemeralWorkspaceRun {
    session: Arc<CommandSession>,
}

/// The isolated workspace run: **many** command sessions sharing the caller's one
/// isolated workspace (namespace + snapshot are owned by the isolated-session
/// subsystem; this run just tracks the command sessions running inside it).
struct IsolatedWorkspaceRun {
    sessions: HashMap<String, Arc<CommandSession>>,
}

/// A caller's workspace runs. The XOR — many ephemeral workspaces (each one
/// session) **or** the one isolated workspace (many sessions) — is enforced by
/// the isolated enter/exit gate and encoded structurally here: an ephemeral
/// caller maps to a set of single-session runs, an isolated caller to one
/// many-session run.
enum CallerRun {
    Ephemeral(HashMap<String, EphemeralWorkspaceRun>),
    Isolated(IsolatedWorkspaceRun),
}

impl CallerRun {
    fn sessions(&self) -> Vec<Arc<CommandSession>> {
        match self {
            Self::Ephemeral(runs) => runs.values().map(|run| Arc::clone(&run.session)).collect(),
            Self::Isolated(run) => run.sessions.values().cloned().collect(),
        }
    }

    fn get(&self, session_id: &str) -> Option<Arc<CommandSession>> {
        match self {
            Self::Ephemeral(runs) => runs.get(session_id).map(|run| Arc::clone(&run.session)),
            Self::Isolated(run) => run.sessions.get(session_id).cloned(),
        }
    }

    fn count(&self) -> usize {
        match self {
            Self::Ephemeral(runs) => runs.len(),
            Self::Isolated(run) => run.sessions.len(),
        }
    }

    /// Remove `session_id`, returning the removed session and whether this caller
    /// run is now empty (so the registry can drop the caller entry).
    fn take(&mut self, session_id: &str) -> (Option<Arc<CommandSession>>, bool) {
        match self {
            Self::Ephemeral(runs) => {
                let removed = runs.remove(session_id).map(|run| run.session);
                (removed, runs.is_empty())
            }
            Self::Isolated(run) => {
                let removed = run.sessions.remove(session_id);
                (removed, run.sessions.is_empty())
            }
        }
    }
}

/// Single caller-keyed command-session authority. Each caller maps to its
/// `CallerRun` (many ephemeral workspace runs or the one isolated run).
/// Session-targeted ops resolve by scanning runs for the session id (caller count
/// is small).
#[derive(Default)]
pub(crate) struct CommandSessionRegistry {
    runs: Mutex<HashMap<String, CallerRun>>,
    completed: Mutex<HashMap<String, CommandSessionCompletion>>,
    counter: AtomicU64,
}

impl CommandSessionRegistry {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub(crate) fn next_id(&self) -> String {
        format!("cmd_{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    /// Place a started session into its caller's runs: a fresh ephemeral workspace
    /// run, or the caller's (created-on-first-session) isolated run.
    pub(crate) fn insert(&self, session: Arc<CommandSession>, kind: WorkspaceRunKind) {
        let caller_id = session.caller_id().to_owned();
        let session_id = session.id().to_owned();
        let mut runs = lock(&self.runs);
        match kind {
            WorkspaceRunKind::Ephemeral => {
                let run = runs
                    .entry(caller_id)
                    .or_insert_with(|| CallerRun::Ephemeral(HashMap::new()));
                if let CallerRun::Ephemeral(ephemeral) = run {
                    ephemeral.insert(session_id, EphemeralWorkspaceRun { session });
                }
            }
            WorkspaceRunKind::Isolated => {
                let run = runs.entry(caller_id).or_insert_with(|| {
                    CallerRun::Isolated(IsolatedWorkspaceRun {
                        sessions: HashMap::new(),
                    })
                });
                if let CallerRun::Isolated(isolated) = run {
                    isolated.sessions.insert(session_id, session);
                }
            }
        }
    }

    #[must_use]
    pub(crate) fn get(&self, id: &str) -> Option<Arc<CommandSession>> {
        lock(&self.runs).values().find_map(|run| run.get(id))
    }

    pub(crate) fn remove(&self, id: &str) -> Option<Arc<CommandSession>> {
        let mut runs = lock(&self.runs);
        let caller = runs
            .iter()
            .find(|(_, run)| run.get(id).is_some())
            .map(|(caller, _)| caller.clone())?;
        let (session, now_empty) = runs
            .get_mut(&caller)
            .map(|run| run.take(id))
            .unwrap_or((None, false));
        if now_empty {
            runs.remove(&caller);
        }
        session
    }

    #[must_use]
    pub(crate) fn count_by_caller(&self, caller_id: Option<&str>) -> usize {
        let runs = lock(&self.runs);
        match caller_id {
            Some(caller) => runs.get(caller).map_or(0, CallerRun::count),
            None => runs.values().map(CallerRun::count).sum(),
        }
    }

    #[must_use]
    pub(crate) fn live(&self) -> Vec<Arc<CommandSession>> {
        lock(&self.runs)
            .values()
            .flat_map(CallerRun::sessions)
            .collect()
    }

    /// All live sessions owned by `caller_id` (drives per-caller cleanup).
    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn caller_sessions(&self, caller_id: &str) -> Vec<Arc<CommandSession>> {
        lock(&self.runs)
            .get(caller_id)
            .map(CallerRun::sessions)
            .unwrap_or_default()
    }

    pub(crate) fn push_completed(&self, completion: CommandSessionCompletion) {
        lock(&self.completed).insert(completion.command_session_id.clone(), completion);
    }

    pub(crate) fn take_completed_result(&self, id: &str) -> Option<CommandResponse> {
        lock(&self.completed).remove(id).map(|entry| entry.result)
    }

    #[must_use]
    pub(crate) fn completed_result(&self, id: &str) -> Option<CommandResponse> {
        lock(&self.completed)
            .get(id)
            .map(|entry| entry.result.clone())
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
            .filter(|(id, completion)| {
                let id_matches = wanted.as_ref().is_none_or(|ids| ids.contains(*id));
                let caller_matches =
                    caller_id.is_none_or(|caller_id| completion.caller_id == caller_id);
                id_matches && caller_matches
            })
            .map(|(id, _)| id.clone())
            .collect();
        let completions = matched
            .iter()
            .filter_map(|id| completed.remove(id))
            .map(|mut completion| {
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

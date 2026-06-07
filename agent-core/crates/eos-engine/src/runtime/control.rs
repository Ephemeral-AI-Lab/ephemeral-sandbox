//! [`AgentRunControl`] — the live object for one agent run (spec §6.1, §6.3).
//!
//! Each root, workflow-agent, and subagent run owns exactly one
//! `AgentRunControl`. It is the object-oriented owner of the run-local
//! cancellation state, foreground executor, notification service, background
//! supervisor, and finalization handles. It replaces `BackgroundRunFinalizer`'s
//! `Drop`-based cleanup as the cleanup owner; teardown is awaited.

use std::sync::{Arc, Mutex};

use eos_state::AgentRunStore;
use eos_types::{AgentRunId, JsonObject, TaskId};
use tokio::sync::Notify;

use super::foreground::ForegroundExecutor;
use crate::background::BackgroundSessionService;
use crate::notifications::NotificationService;
use crate::EngineError;

/// The cooperative half of cancellation (spec §6.1).
///
/// The query loop polls it at turn boundaries; once a cancel is requested it
/// prevents future work from starting. It does **not** clean up already-spawned
/// effects — that is owned by the foreground/background teardown paths. Provider
/// streams are not treated as cancel-safe and are never interrupted mid-response.
#[derive(Clone, Debug)]
pub struct AgentRunCancellation {
    state: Arc<AgentRunCancellationState>,
}

#[derive(Debug)]
struct AgentRunCancellationState {
    cancelled: std::sync::atomic::AtomicBool,
    reason: Mutex<Option<String>>,
    notify: Notify,
}

impl Default for AgentRunCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunCancellation {
    /// A fresh, not-yet-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(AgentRunCancellationState {
                cancelled: std::sync::atomic::AtomicBool::new(false),
                reason: Mutex::new(None),
                notify: Notify::new(),
            }),
        }
    }

    /// Request cancellation, recording the first reason. Idempotent.
    pub fn request_cancel(&self, reason: impl Into<String>) {
        let mut guard = self.state.reason.lock().expect("cancellation reason lock");
        if guard.is_none() {
            *guard = Some(reason.into());
        }
        drop(guard);
        self.state
            .cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancel_requested(&self) -> bool {
        self.state
            .cancelled
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The recorded cancellation reason, if any.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.state
            .reason
            .lock()
            .expect("cancellation reason lock")
            .clone()
    }

    /// Resolve once cancellation has been requested.
    pub async fn wait_for_cancel(&self) {
        if self.is_cancel_requested() {
            return;
        }
        self.state.notify.notified().await;
    }
}

/// Whether an agent run persists a durable `agent_run` row.
#[derive(Debug, Clone)]
pub enum AgentRunPersistence {
    /// Task-backed root / workflow-agent run: owns a durable `AgentRunStore`
    /// completion obligation.
    Persisted {
        /// The owning task.
        task_id: TaskId,
    },
    /// Live-only subagent / helper run: no `AgentRunStore` row to create/finish.
    Ephemeral,
}

/// Finalization data and the cancelled-finish path for one agent run (spec §6.3).
///
/// Finalization owns only the **durable `agent_run` row** completion. The
/// message-record handle is deliberately *not* held here: it stays in the
/// `QueryContext` and is always finished by `run_agent` itself (cancel-aware
/// status), since the loop is awaited rather than aborted on cancellation. This
/// keeps record ownership in one place and avoids cloning the record handle into
/// the control (a documented, observably-equivalent divergence from §12.3's
/// literal "`finish_cancelled` finishes the message-record": finished once, and
/// cancel-aware either way).
pub struct AgentRunFinalization {
    agent_run_id: AgentRunId,
    persistence: AgentRunPersistence,
    agent_run_store: Arc<dyn AgentRunStore>,
}

impl std::fmt::Debug for AgentRunFinalization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunFinalization")
            .field("agent_run_id", &self.agent_run_id)
            .field("persistence", &self.persistence)
            .finish_non_exhaustive()
    }
}

impl AgentRunFinalization {
    fn new(
        agent_run_id: AgentRunId,
        persistence: AgentRunPersistence,
        agent_run_store: Arc<dyn AgentRunStore>,
    ) -> Self {
        Self {
            agent_run_id,
            persistence,
            agent_run_store,
        }
    }

    /// The owning task id, for persisted runs.
    #[must_use]
    pub fn task_id(&self) -> Option<&TaskId> {
        match &self.persistence {
            AgentRunPersistence::Persisted { task_id } => Some(task_id),
            AgentRunPersistence::Ephemeral => None,
        }
    }

    /// Finish the run's durable `agent_run` row as cancelled. Ephemeral runs own
    /// no row, so this is a no-op for them. The message-record handle is finished
    /// by `run_agent`, not here.
    pub async fn finish_cancelled(&self, reason: &str) -> Result<(), EngineError> {
        if matches!(self.persistence, AgentRunPersistence::Persisted { .. }) {
            let mut terminal = JsonObject::new();
            terminal.insert("fail_reason".to_owned(), "cancelled".into());
            terminal.insert("reason".to_owned(), reason.into());
            self.agent_run_store
                .finish_run(&self.agent_run_id, None, Some(&terminal), 0, Some(reason))
                .await?;
        }
        Ok(())
    }
}

/// The live object for one agent run (spec §6.3).
///
/// The run's command-session completion monitor is owned by
/// `BackgroundSessionRuntime`, so dropping this control (the last service clone)
/// drops the runtime and aborts the monitors (RAII) — no separate guard is held
/// here.
pub struct AgentRunControl {
    agent_run_id: AgentRunId,
    cancellation: AgentRunCancellation,
    foreground: Arc<ForegroundExecutor>,
    notifications: NotificationService,
    background: BackgroundSessionService,
    finalization: AgentRunFinalization,
}

impl std::fmt::Debug for AgentRunControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunControl")
            .field("agent_run_id", &self.agent_run_id)
            .finish_non_exhaustive()
    }
}

impl AgentRunControl {
    /// Assemble a control from its already-built parts. Called only by
    /// `AgentRunControlFactory`.
    pub(super) fn assemble(parts: AgentRunControlParts) -> Self {
        let AgentRunControlParts {
            agent_run_id,
            persistence,
            agent_run_store,
            foreground,
            notifications,
            background,
        } = parts;
        Self {
            cancellation: AgentRunCancellation::new(),
            foreground,
            notifications,
            background,
            finalization: AgentRunFinalization::new(
                agent_run_id.clone(),
                persistence,
                agent_run_store,
            ),
            agent_run_id,
        }
    }

    /// The agent-run id this control owns.
    #[must_use]
    pub fn agent_run_id(&self) -> &AgentRunId {
        &self.agent_run_id
    }

    /// The owning task id, for persisted runs.
    #[must_use]
    pub fn task_id(&self) -> Option<&TaskId> {
        self.finalization.task_id()
    }

    /// A clone of the run's cancellation token (shared with the query loop).
    #[must_use]
    pub fn cancellation(&self) -> AgentRunCancellation {
        self.cancellation.clone()
    }

    /// A clone of the run's background session service.
    #[must_use]
    pub fn background(&self) -> BackgroundSessionService {
        self.background.clone()
    }

    /// A clone of the run-local notification service (the exact queue the loop
    /// drains and the background managers enqueue into — the §7 instance-identity
    /// invariant.
    #[must_use]
    pub fn notifications(&self) -> NotificationService {
        self.notifications.clone()
    }

    /// The run's foreground executor (shared into the query loop / tools).
    #[must_use]
    pub fn foreground(&self) -> Arc<ForegroundExecutor> {
        self.foreground.clone()
    }

    /// The run's finalization handles (used by the cancellation path).
    #[must_use]
    pub fn finalization(&self) -> &AgentRunFinalization {
        &self.finalization
    }
}

/// The already-built parts an [`AgentRunControl`] is assembled from.
pub(super) struct AgentRunControlParts {
    pub(super) agent_run_id: AgentRunId,
    pub(super) persistence: AgentRunPersistence,
    pub(super) agent_run_store: Arc<dyn AgentRunStore>,
    pub(super) foreground: Arc<ForegroundExecutor>,
    pub(super) notifications: NotificationService,
    pub(super) background: BackgroundSessionService,
}

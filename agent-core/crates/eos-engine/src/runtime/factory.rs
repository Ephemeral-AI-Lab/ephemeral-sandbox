//! [`AgentRunControlFactory`] — the request-scoped composition helper that builds
//! one fresh [`AgentRunControl`] per root / workflow / subagent run (spec §6.2).
//!
//! The factory is **not** per agent run: it is created once per request/workspace
//! composition and reused. It holds only immutable construction capability
//! (the foreground and background factories) and must never retain a live
//! `AgentRunControl`, `NotificationService`, `ForegroundExecutor`,
//! `BackgroundSupervisorHandle`, or lane records. Each call mints a fresh
//! notification service, foreground executor, background supervisor, and
//! command-completion heartbeat.

use std::sync::Arc;

use eos_types::{AgentRunId, TaskId};

use crate::background::BackgroundSupervisorFactory;
use crate::notifications::NotificationService;

use super::control::{AgentRunControl, AgentRunControlParts, AgentRunPersistence};
use super::foreground::ForegroundExecutorFactory;

/// Request-scoped, cloneable factory for per-agent-run [`AgentRunControl`]s.
#[derive(Clone)]
pub struct AgentRunControlFactory {
    foreground: ForegroundExecutorFactory,
    background: BackgroundSupervisorFactory,
}

impl std::fmt::Debug for AgentRunControlFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunControlFactory").finish_non_exhaustive()
    }
}

impl AgentRunControlFactory {
    /// Compose the factory from the per-request foreground and background
    /// builders.
    #[must_use]
    pub fn new(foreground: ForegroundExecutorFactory, background: BackgroundSupervisorFactory) -> Self {
        Self {
            foreground,
            background,
        }
    }

    /// Build a control for a task-backed root or workflow-agent run. The run owns
    /// a durable `agent_run` completion obligation.
    #[must_use]
    pub fn persisted(&self, agent_run_id: AgentRunId, task_id: TaskId) -> Arc<AgentRunControl> {
        self.build(agent_run_id, AgentRunPersistence::Persisted { task_id })
    }

    /// Build a control for a live-only subagent / helper run that still needs
    /// local cancellation, background cleanup, and message-record finalization,
    /// but must not create or finish an `agent_run` row.
    #[must_use]
    pub fn ephemeral(&self, agent_run_id: AgentRunId) -> Arc<AgentRunControl> {
        self.build(agent_run_id, AgentRunPersistence::Ephemeral)
    }

    /// Must be called within a Tokio runtime: it spawns the run's
    /// command-completion heartbeat.
    fn build(&self, agent_run_id: AgentRunId, persistence: AgentRunPersistence) -> Arc<AgentRunControl> {
        let notifications = NotificationService::new();
        let foreground = Arc::new(self.foreground.create(agent_run_id.clone()));
        // The handle carries a clone of this factory so its `spawn` can mint each
        // subagent its own ephemeral control (spec §8.1/§11.3). This is value
        // capability only — the factory holds no `AgentRunControl`, so there is
        // no reference cycle.
        let background = self.background.create(self.clone());
        let heartbeat = self.background.spawn_heartbeat(&background, &notifications);
        Arc::new(AgentRunControl::assemble(AgentRunControlParts {
            agent_run_id,
            persistence,
            agent_run_store: self.background.agent_run_store(),
            foreground,
            notifications,
            background,
            heartbeat,
        }))
    }
}

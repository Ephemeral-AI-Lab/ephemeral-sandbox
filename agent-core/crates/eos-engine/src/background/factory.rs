//! [`BackgroundSupervisorFactory`] — the request-scoped builder of one
//! per-agent-run [`BackgroundSupervisorHandle`] (spec §8.1).
//!
//! Owned by the request-scoped `AgentRunControlFactory`. It is immutable and
//! cheap to clone, and holds only the immutable construction dependencies (run
//! handles, sandbox transport, completion poll interval) — never a per-agent
//! ledger. Each `create` mints a fresh per-run supervisor; `spawn_heartbeat`
//! starts that run's command-completion heartbeat against the run's own
//! notification service.

use std::sync::Arc;
use std::time::Duration;

use eos_sandbox_port::SandboxTransport;
use eos_tools::NotificationSink;
use tokio::task::JoinHandle;

use super::handle::BackgroundSupervisorHandle;
use super::heartbeat::spawn_command_completion_heartbeat;
use crate::notifications::NotificationService;
use crate::runtime::AgentRunControlFactory;
use crate::EngineRunHandles;

/// Request-scoped, immutable factory for per-agent-run background supervisors.
#[derive(Clone)]
pub struct BackgroundSupervisorFactory {
    handles: EngineRunHandles,
    transport: Arc<dyn SandboxTransport>,
    completion_poll_interval: Duration,
}

impl std::fmt::Debug for BackgroundSupervisorFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSupervisorFactory")
            .field("completion_poll_interval", &self.completion_poll_interval)
            .finish_non_exhaustive()
    }
}

impl BackgroundSupervisorFactory {
    /// Build the factory from the immutable per-request construction inputs.
    #[must_use]
    pub fn new(
        handles: EngineRunHandles,
        transport: Arc<dyn SandboxTransport>,
        completion_poll_interval: Duration,
    ) -> Self {
        Self {
            handles,
            transport,
            completion_poll_interval,
        }
    }

    /// Mint a fresh per-agent-run background supervisor handle with an empty
    /// ledger. `control_factory` is the request-scoped factory clone the handle
    /// uses to give each subagent its own ephemeral control (spec §8.1/§11.3).
    #[must_use]
    pub fn create(&self, control_factory: AgentRunControlFactory) -> BackgroundSupervisorHandle {
        BackgroundSupervisorHandle::new(self.handles.clone(), self.transport.clone(), control_factory)
    }

    /// The durable agent-run store, used by a control's finalization to finish a
    /// persisted run as cancelled.
    #[must_use]
    pub(crate) fn agent_run_store(&self) -> Arc<dyn eos_state::AgentRunStore> {
        self.handles.agent_run_store.clone()
    }

    /// Spawn this run's command-completion heartbeat. The returned join handle is
    /// owned (and aborted on drop) by the run's `AgentRunControl`; the heartbeat
    /// enqueues completions into the run's own `notifications` queue — the same
    /// instance the query loop drains.
    #[must_use]
    pub(crate) fn spawn_heartbeat(
        &self,
        background: &BackgroundSupervisorHandle,
        notifications: &NotificationService,
    ) -> JoinHandle<()> {
        let sink: Arc<dyn NotificationSink> = Arc::new(notifications.clone());
        spawn_command_completion_heartbeat(
            background.inner(),
            sink,
            self.transport.clone(),
            self.completion_poll_interval,
        )
    }
}

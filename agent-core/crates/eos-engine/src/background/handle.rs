//! [`BackgroundSupervisorHandle`] — the async wrapper over
//! [`BackgroundTaskSupervisor`] that the subagent driver and command-session
//! tools call through. It is the real
//! [`BackgroundSupervisorPort`](eos_tools::ports::BackgroundSupervisorPort)
//! (impl in `subagent.rs`) and
//! [`CommandSessionSupervisorPort`](eos_tools::ports::CommandSessionSupervisorPort)
//! (impl in `command_session.rs`), holds the [`EngineRunHandles`] the subagent
//! driver needs, and owns the parent-exit cleanup path.

use std::sync::Arc;

use eos_sandbox_api::{CommandSessionCancelRequest, SandboxRequestBase, SandboxTransport};
use eos_tools::ports::Sealed;
use eos_tools::{BackgroundInflightReport, WorkflowControlPort};
use eos_types::AgentRunId;
use tokio::sync::Mutex;

use super::supervisor::{BackgroundTaskSupervisor, CommandSessionCancelTarget};
use crate::EngineRunHandles;

/// The run dependencies the subagent driver needs, threaded in at the
/// composition root: the engine run handles (registry + stores + client + workspace root).
#[derive(Clone)]
pub struct BackgroundSupervisorHandle {
    inner: Arc<Mutex<BackgroundTaskSupervisor>>,
    pub(super) handles: EngineRunHandles,
    transport: Arc<dyn SandboxTransport>,
}

impl std::fmt::Debug for BackgroundSupervisorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSupervisorHandle")
            .finish_non_exhaustive()
    }
}

impl BackgroundSupervisorHandle {
    /// Create the shared supervisor with the run handles the subagent driver
    /// needs. The ledger starts empty.
    #[must_use]
    pub fn new(handles: EngineRunHandles, transport: Arc<dyn SandboxTransport>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackgroundTaskSupervisor::new())),
            handles,
            transport,
        }
    }

    /// Access the shared supervisor state for the heartbeat and runtime adapters.
    #[must_use]
    pub fn inner(&self) -> Arc<Mutex<BackgroundTaskSupervisor>> {
        self.inner.clone()
    }

    /// Cancel all background work tracked for one parent agent run. This is the
    /// common parent-exit finalizer for `ToolStop`, terminal exhaustion, and
    /// engine faults.
    pub async fn cancel_for_parent_exit(
        &self,
        agent_run_id: Option<&AgentRunId>,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> BackgroundInflightReport {
        let (workflows, commands) = {
            let mut guard = self.inner.lock().await;
            if let Some(agent_run_id) = agent_run_id {
                guard.cancel_subagents_for_agent_run(agent_run_id);
            }
            (
                guard.running_workflows_for_agent_run(agent_run_id),
                guard.running_commands_for_agent_run(agent_run_id),
            )
        };

        for workflow_task_id in workflows {
            if let Some(control) = &workflow_control {
                if let Err(err) = control.cancel(&workflow_task_id, reason).await {
                    tracing::warn!(
                        error = %err,
                        workflow_task_id = workflow_task_id.as_str(),
                        "background workflow parent-exit cancellation failed"
                    );
                }
            }
            self.inner
                .lock()
                .await
                .cancel_workflow_record(&workflow_task_id, reason);
        }

        for command in commands {
            self.cancel_command_session_for_parent_exit(&command, reason)
                .await;
            self.inner
                .lock()
                .await
                .cancel_command_record(&command.command_session_id);
        }

        self.inner.lock().await.inflight_report(agent_run_id)
    }

    async fn cancel_command_session_for_parent_exit(
        &self,
        command: &CommandSessionCancelTarget,
        reason: &str,
    ) {
        let request = CommandSessionCancelRequest {
            base: SandboxRequestBase::new(
                command.agent_run_id.as_str(),
                format!("parent-exit cleanup: {reason}"),
                None,
            ),
            command_session_id: command.command_session_id.clone(),
        };
        if let Err(err) =
            eos_sandbox_api::cancel_command_session(&*self.transport, &command.sandbox_id, &request)
                .await
        {
            tracing::warn!(
                error = %err,
                command_session_id = command.command_session_id.as_str(),
                "background command-session parent-exit cancellation failed"
            );
        }
    }
}

impl Sealed for BackgroundSupervisorHandle {}

//! Runtime-local recursive cancellation port.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_agent_ports::AgentRunApi;
use eos_tool_ports::{CancelPort, ToolError};
use eos_types::{AgentRunId, TaskId};
use eos_types::{AgentRunStore, JsonObject, TaskStatus, TaskStore};

/// Request-scoped cancellation port wired to the runner service.
#[derive(Clone)]
pub(crate) struct RuntimeCancelPort {
    task_store: Arc<dyn TaskStore>,
    agent_run_store: Arc<dyn AgentRunStore>,
    agent_run_api: Arc<OnceLock<Arc<dyn AgentRunApi>>>,
}

impl std::fmt::Debug for RuntimeCancelPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeCancelPort").finish_non_exhaustive()
    }
}

impl RuntimeCancelPort {
    pub(crate) fn new(
        task_store: Arc<dyn TaskStore>,
        agent_run_store: Arc<dyn AgentRunStore>,
        agent_run_api: Arc<OnceLock<Arc<dyn AgentRunApi>>>,
    ) -> Self {
        Self {
            task_store,
            agent_run_store,
            agent_run_api,
        }
    }

    async fn cancel_live_run_for_task(
        &self,
        task_id: &TaskId,
        reason: &str,
    ) -> Result<(), ToolError> {
        let Some(run) = self.agent_run_store.get_for_task(task_id).await? else {
            return Ok(());
        };
        if run.finished_at.is_some() {
            return Ok(());
        }
        self.cancel_agent_run(&run.id, reason).await
    }
}

#[async_trait]
impl CancelPort for RuntimeCancelPort {
    async fn cancel_task(&self, task_id: &TaskId, reason: &str) -> Result<(), ToolError> {
        if let Some(task) = self.task_store.get(task_id).await? {
            if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                let terminal = cancelled_terminal(reason);
                self.task_store
                    .set_task_status_if_current(
                        task_id,
                        task.status,
                        TaskStatus::Cancelled,
                        None,
                        Some(&terminal),
                    )
                    .await?;
            }
        }
        self.cancel_live_run_for_task(task_id, reason).await
    }

    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), ToolError> {
        let Some(agent_run_api) = self.agent_run_api.get().cloned() else {
            return Ok(());
        };
        agent_run_api
            .cancel_agent_run(agent_run_id, reason)
            .await
            .map_err(|err| ToolError::Internal(err.to_string()))
    }
}

fn cancelled_terminal(reason: &str) -> JsonObject {
    let mut terminal = JsonObject::new();
    terminal.insert("fail_reason".to_owned(), "cancelled".into());
    terminal.insert("reason".to_owned(), reason.into());
    terminal
}

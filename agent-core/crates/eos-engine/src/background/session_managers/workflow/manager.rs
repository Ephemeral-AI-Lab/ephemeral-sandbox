use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_tools::{StartedWorkflowHandle, WorkflowControlPort};
use eos_types::{WorkflowId, WorkflowSessionId};
use tokio::sync::Mutex;

use super::super::{BackgroundSession, BackgroundSessionManager, BackgroundSessionStatus};
use super::session::WorkflowSession;
use crate::background::notification::{BackgroundCompletion, BackgroundNotificationEmitter};

pub(in crate::background) type WorkflowControlCell = Arc<OnceLock<Arc<dyn WorkflowControlPort>>>;

#[derive(Debug, Clone)]
pub(in crate::background) struct WorkflowCompletion {
    pub(super) workflow_task_id: WorkflowSessionId,
    pub(super) workflow_id: WorkflowId,
    pub(super) status: BackgroundSessionStatus,
}

/// Tracks delegated workflow sessions for one agent run.
#[derive(Clone)]
pub(in crate::background) struct WorkflowSessionManager {
    sessions: Arc<Mutex<HashMap<WorkflowSessionId, WorkflowSession>>>,
    workflow_port: WorkflowControlCell,
    notification: BackgroundNotificationEmitter,
}

impl std::fmt::Debug for WorkflowSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowSessionManager")
            .finish_non_exhaustive()
    }
}

impl WorkflowSessionManager {
    pub(in crate::background) fn new(
        workflow_port: WorkflowControlCell,
        notification: BackgroundNotificationEmitter,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            workflow_port,
            notification,
        }
    }

    pub(in crate::background) async fn register(&self, workflow: &StartedWorkflowHandle) {
        self.insert(WorkflowSession::running(
            workflow.workflow_task_id.clone(),
            workflow.workflow_id.clone(),
        ))
        .await;
    }

    pub(in crate::background) async fn cancel_record(
        &self,
        workflow_task_id: &WorkflowSessionId,
    ) -> bool {
        self.sessions
            .lock()
            .await
            .get_mut(workflow_task_id)
            .is_some_and(WorkflowSession::cancel)
    }

    pub(in crate::background) async fn cancel_with_port(
        &self,
        workflow_port: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) {
        let workflow_port = workflow_port.or_else(|| self.workflow_port.get().cloned());
        let running = self.running_ids().await;
        for workflow_task_id in &running {
            if let Some(port) = &workflow_port {
                if let Err(err) = port.cancel(workflow_task_id, reason).await {
                    tracing::warn!(
                        error = %err,
                        workflow_task_id = workflow_task_id.as_str(),
                        "background workflow cancellation failed"
                    );
                }
            }
            let _ = self.cancel_record(workflow_task_id).await;
        }
    }

    pub(super) async fn running_ids(&self) -> Vec<WorkflowSessionId> {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|session| matches!(session.status(), BackgroundSessionStatus::Running))
            .map(|session| session.id().clone())
            .collect()
    }

    async fn running_sessions(&self) -> Vec<WorkflowSession> {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|session| matches!(session.status(), BackgroundSessionStatus::Running))
            .cloned()
            .collect()
    }

    async fn settle_running(
        &self,
        workflow_task_id: &WorkflowSessionId,
        status: BackgroundSessionStatus,
    ) -> Option<WorkflowCompletion> {
        let mut guard = self.sessions.lock().await;
        let session = guard.get_mut(workflow_task_id)?;
        if !session.settle_running(status) {
            return None;
        }
        Some(WorkflowCompletion {
            workflow_task_id: session.id().clone(),
            workflow_id: session.workflow_id().clone(),
            status,
        })
    }
}

#[async_trait]
impl BackgroundSessionManager for WorkflowSessionManager {
    type Session = WorkflowSession;
    type Completion = WorkflowCompletion;

    async fn insert(&self, session: Self::Session) {
        self.sessions
            .lock()
            .await
            .insert(session.id().clone(), session);
    }

    async fn count(&self) -> usize {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|session| matches!(session.status(), BackgroundSessionStatus::Running))
            .count()
    }

    async fn poll(&self) -> Vec<Self::Completion> {
        let Some(workflow_port) = self.workflow_port.get().cloned() else {
            return Vec::new();
        };
        let mut completions = Vec::new();
        for session in self.running_sessions().await {
            let status_text = match workflow_port
                .status(session.workflow_id(), Some(session.id()))
                .await
            {
                Ok(text) => text,
                Err(_) => continue,
            };
            let Some(status) = terminal_status(&status_text) else {
                continue;
            };
            if let Some(completion) = self.settle_running(session.id(), status).await {
                completions.push(completion);
            }
        }
        completions
    }

    async fn finish(&self, completion: Self::Completion) {
        let _ = self
            .notification
            .emit(BackgroundCompletion::Workflow {
                workflow_task_id: completion.workflow_task_id,
                workflow_id: completion.workflow_id,
                status: completion.status,
            })
            .await;
    }

    async fn cancel(&self, reason: &str) {
        self.cancel_with_port(None, reason).await;
    }
}

pub(super) fn terminal_status(status_text: &str) -> Option<BackgroundSessionStatus> {
    if status_text.contains("is Succeeded.") {
        Some(BackgroundSessionStatus::Completed)
    } else if status_text.contains("is Failed.") {
        Some(BackgroundSessionStatus::Failed)
    } else if status_text.contains("is Cancelled.") {
        Some(BackgroundSessionStatus::Cancelled)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::{Arc, OnceLock};

    use async_trait::async_trait;
    use eos_state::TaskId;
    use eos_tools::{OutstandingWorkflow, StartedWorkflowHandle, ToolError};
    use eos_types::AgentRunId;

    use crate::background::notification::BackgroundNotificationEmitter;
    use crate::background::session_managers::BackgroundSessionManager;
    use crate::NotificationService;

    use super::*;

    #[derive(Debug)]
    struct AlwaysSucceededControl;

    impl eos_tools::ports::Sealed for AlwaysSucceededControl {}

    #[async_trait]
    impl WorkflowControlPort for AlwaysSucceededControl {
        async fn start(
            &self,
            _parent_task_id: &TaskId,
            _agent_run_id: &AgentRunId,
            _workflow_goal: &str,
        ) -> Result<StartedWorkflowHandle, ToolError> {
            unreachable!("not used")
        }

        async fn status(
            &self,
            workflow_id: &WorkflowId,
            workflow_task_id: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            let handle = workflow_task_id.map_or("?", WorkflowSessionId::as_str);
            Ok(format!(
                "Workflow {workflow_id} ({handle}) is Succeeded. Goal: x"
            ))
        }

        async fn cancel(
            &self,
            _workflow_task_id: &WorkflowSessionId,
            _reason: &str,
        ) -> Result<String, ToolError> {
            Ok("cancelled".to_owned())
        }

        async fn find_outstanding(
            &self,
            _parent_task_id: &TaskId,
            _agent_run_id: &AgentRunId,
        ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
            Ok(Vec::new())
        }

        async fn workflow_depth(&self, _workflow_id: &WorkflowId) -> Result<u32, ToolError> {
            Ok(1)
        }
    }

    fn manager(notifier: &NotificationService) -> WorkflowSessionManager {
        let cell: WorkflowControlCell = Arc::new(OnceLock::new());
        let _ = cell.set(Arc::new(AlwaysSucceededControl));
        WorkflowSessionManager::new(cell, BackgroundNotificationEmitter::new(notifier.clone()))
    }

    #[test]
    fn terminal_status_parses_renderings() {
        assert_eq!(
            terminal_status("Workflow w1 (wf_1) is Succeeded. Goal: x"),
            Some(BackgroundSessionStatus::Completed)
        );
        assert_eq!(
            terminal_status("Workflow w1 (wf_1) is Failed. Goal: x"),
            Some(BackgroundSessionStatus::Failed)
        );
        assert_eq!(
            terminal_status("Workflow w1 (wf_1) is Cancelled. Goal: x"),
            Some(BackgroundSessionStatus::Cancelled)
        );
        assert_eq!(terminal_status("Workflow w1 (wf_1) is Open. Goal: x"), None);
        assert_eq!(terminal_status("Workflow wf_1 was not found."), None);
    }

    #[tokio::test]
    async fn poll_finish_and_cancel_are_manager_owned() {
        let notifier = NotificationService::new();
        let manager = manager(&notifier);
        manager
            .register(&StartedWorkflowHandle {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: "wf_1".parse().expect("workflow handle"),
            })
            .await;
        assert_eq!(manager.count().await, 1);

        let completions = manager.poll().await;
        assert_eq!(completions.len(), 1);
        for completion in completions {
            manager.finish(completion).await;
        }
        assert_eq!(manager.count().await, 0);
        let notifications = notifier.drain().await;
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0]
            .message
            .contains("[BACKGROUND COMPLETED] workflow_task_id=wf_1"));

        manager
            .register(&StartedWorkflowHandle {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: "wf_2".parse().expect("workflow handle"),
            })
            .await;
        assert_eq!(manager.count().await, 1);
        manager.cancel("parent exited").await;
        assert_eq!(manager.count().await, 0);
    }
}

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use eos_tool_core::{Sealed, StartedWorkflow, WorkflowServicePort, WorkflowSessionPort};
use eos_types::{AgentRunId, WorkflowId, WorkflowSessionId};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{BackgroundSession, BackgroundSessionManager, BackgroundSessionStatus};
use crate::background::notification::{BackgroundCompletion, BackgroundNotificationEmitter};

pub(in crate::background) type WorkflowServiceCell = Arc<OnceLock<Arc<dyn WorkflowServicePort>>>;

/// One delegated workflow tracked as background work for the owning agent run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::background) struct WorkflowSession {
    id: WorkflowSessionId,
    workflow_id: WorkflowId,
    status: BackgroundSessionStatus,
}

impl WorkflowSession {
    fn running(id: WorkflowSessionId, workflow_id: WorkflowId) -> Self {
        Self {
            id,
            workflow_id,
            status: BackgroundSessionStatus::Running,
        }
    }

    fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    const fn status(&self) -> BackgroundSessionStatus {
        self.status
    }

    fn cancel(&mut self) -> bool {
        if !matches!(self.status, BackgroundSessionStatus::Running) {
            return false;
        }
        self.status = BackgroundSessionStatus::Cancelled;
        true
    }

    fn settle_running(&mut self, status: BackgroundSessionStatus) -> bool {
        if !matches!(self.status, BackgroundSessionStatus::Running) {
            return false;
        }
        self.status = status;
        true
    }
}

impl BackgroundSession for WorkflowSession {
    type Id = WorkflowSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}

#[derive(Debug, Clone)]
pub(in crate::background) struct WorkflowCompletion {
    pub(super) workflow_task_id: WorkflowSessionId,
    pub(super) workflow_id: WorkflowId,
    pub(super) status: BackgroundSessionStatus,
}

/// Tracks delegated workflow sessions for one agent run.
#[derive(Clone)]
pub(in crate::background) struct WorkflowSessionManager {
    agent_run_id: AgentRunId,
    sessions: Arc<Mutex<HashMap<WorkflowSessionId, WorkflowSession>>>,
    workflow_service: WorkflowServiceCell,
    notification: BackgroundNotificationEmitter,
}

impl std::fmt::Debug for WorkflowSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowSessionManager")
            .field("agent_run_id", &self.agent_run_id)
            .finish_non_exhaustive()
    }
}

impl WorkflowSessionManager {
    pub(in crate::background) fn new(
        agent_run_id: AgentRunId,
        workflow_service: WorkflowServiceCell,
        notification: BackgroundNotificationEmitter,
    ) -> Self {
        Self {
            agent_run_id,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            workflow_service,
            notification,
        }
    }

    pub(in crate::background) async fn register_background_session(
        &self,
        workflow: &StartedWorkflow,
    ) {
        self.insert(WorkflowSession::running(
            workflow.workflow_task_id.clone(),
            workflow.workflow_id.clone(),
        ))
        .await;
    }

    pub(in crate::background) async fn cancel_session(
        &self,
        workflow_task_id: &WorkflowSessionId,
    ) -> bool {
        self.sessions
            .lock()
            .await
            .get_mut(workflow_task_id)
            .is_some_and(WorkflowSession::cancel)
    }

    pub(in crate::background) async fn cancel_background_sessions(&self, reason: &str) {
        let workflow_service = self.workflow_service.get().cloned();
        let running = self.running_ids().await;
        for workflow_task_id in &running {
            if let Some(service) = &workflow_service {
                if let Err(err) = service
                    .cancel_workflow_session(workflow_task_id, reason)
                    .await
                {
                    tracing::warn!(
                        error = %err,
                        workflow_task_id = workflow_task_id.as_str(),
                        "background workflow cancellation failed"
                    );
                }
            }
            let _ = self.cancel_session(workflow_task_id).await;
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

    pub(in crate::background) async fn poll_completions(&self) -> Vec<WorkflowCompletion> {
        let Some(workflow_service) = self.workflow_service.get().cloned() else {
            return Vec::new();
        };
        let mut completions = Vec::new();
        for session in self.running_sessions().await {
            let terminal = match workflow_service
                .poll_terminal_workflow(session.workflow_id(), session.id())
                .await
            {
                Ok(terminal) => terminal,
                Err(_) => continue,
            };
            let Some(terminal) = terminal else {
                continue;
            };
            let status = match terminal.status {
                eos_tool_core::SubagentSessionStatus::Completed => BackgroundSessionStatus::Completed,
                eos_tool_core::SubagentSessionStatus::Failed => BackgroundSessionStatus::Failed,
                eos_tool_core::SubagentSessionStatus::Cancelled => BackgroundSessionStatus::Cancelled,
                eos_tool_core::SubagentSessionStatus::Running
                | eos_tool_core::SubagentSessionStatus::Delivered => continue,
            };
            if let Some(completion) = self.settle_running(session.id(), status).await {
                completions.push(completion);
            }
        }
        completions
    }
}

pub(in crate::background) struct WorkflowSessionMonitor {
    join: JoinHandle<()>,
}

impl Drop for WorkflowSessionMonitor {
    fn drop(&mut self) {
        self.join.abort();
    }
}

impl WorkflowSessionMonitor {
    pub(in crate::background) fn spawn(
        manager: WorkflowSessionManager,
        interval: Duration,
    ) -> Self {
        Self {
            join: tokio::spawn(async move {
                loop {
                    for completion in manager.poll_completions().await {
                        manager.push_notification_on_completion(completion).await;
                    }
                    tokio::time::sleep(interval).await;
                }
            }),
        }
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

    async fn push_notification_on_completion(&self, completion: Self::Completion) {
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
        self.cancel_background_sessions(reason).await;
    }
}

impl Sealed for WorkflowSessionManager {}

#[async_trait]
impl WorkflowSessionPort for WorkflowSessionManager {
    async fn register_background_session(&self, workflow: &StartedWorkflow) {
        WorkflowSessionManager::register_background_session(self, workflow).await;
    }

    async fn count_background_sessions(&self) -> usize {
        BackgroundSessionManager::count(self).await
    }

    async fn cancel_all_background_sessions(&self, reason: &str) {
        BackgroundSessionManager::cancel(self, reason).await;
    }

    async fn poll_complete_background_sessions(&self) -> usize {
        let completions = self.poll_completions().await;
        let count = completions.len();
        for completion in completions {
            self.push_notification_on_completion(completion).await;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::{Arc, OnceLock};

    use async_trait::async_trait;
    use eos_tool_core::{
        OutstandingWorkflow, StartWorkflowRequest, StartedWorkflow, SubagentSessionStatus,
        TerminalWorkflow, ToolError, WorkflowServicePort,
    };
    use eos_state::TaskId;
    use eos_types::AgentRunId;

    use crate::background::notification::BackgroundNotificationEmitter;
    use crate::NotificationService;

    use super::*;

    #[derive(Debug)]
    struct AlwaysSucceededService;

    impl Sealed for AlwaysSucceededService {}

    #[async_trait]
    impl WorkflowServicePort for AlwaysSucceededService {
        async fn start_workflow(
            &self,
            _request: StartWorkflowRequest,
        ) -> Result<StartedWorkflow, ToolError> {
            unreachable!("not used")
        }

        async fn check_workflow_status(
            &self,
            _workflow_id: &WorkflowId,
            _workflow_task_id: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            unreachable!("not used")
        }

        async fn cancel_workflow_session(
            &self,
            _workflow_task_id: &WorkflowSessionId,
            _reason: &str,
        ) -> Result<String, ToolError> {
            Ok("cancelled".to_owned())
        }

        async fn poll_terminal_workflow(
            &self,
            workflow_id: &WorkflowId,
            workflow_task_id: &WorkflowSessionId,
        ) -> Result<Option<TerminalWorkflow>, ToolError> {
            Ok(Some(TerminalWorkflow {
                workflow_id: workflow_id.clone(),
                workflow_task_id: workflow_task_id.clone(),
                status: SubagentSessionStatus::Completed,
            }))
        }

        async fn find_outstanding_workflows(
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
        let cell: WorkflowServiceCell = Arc::new(OnceLock::new());
        let _ = cell.set(Arc::new(AlwaysSucceededService));
        WorkflowSessionManager::new(
            "owner-run".parse().expect("agent run id"),
            cell,
            BackgroundNotificationEmitter::new(notifier.clone()),
        )
    }

    #[tokio::test]
    async fn poll_push_notification_and_cancel_are_manager_owned() {
        let notifier = NotificationService::new();
        let manager = manager(&notifier);
        manager
            .register_background_session(&StartedWorkflow {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: "wf_1".parse().expect("workflow session"),
            })
            .await;
        assert_eq!(manager.count().await, 1);

        let completions = manager.poll_completions().await;
        assert_eq!(completions.len(), 1);
        for completion in completions {
            manager.push_notification_on_completion(completion).await;
        }
        assert_eq!(manager.count().await, 0);
        let notifications = notifier.drain().await;
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0]
            .message
            .contains("[BACKGROUND COMPLETED] workflow_task_id=wf_1"));

        manager
            .register_background_session(&StartedWorkflow {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: "wf_2".parse().expect("workflow session"),
            })
            .await;
        assert_eq!(manager.count().await, 1);
        manager.cancel("parent exited").await;
        assert_eq!(manager.count().await, 0);
    }
}

mod command_session_manager;
mod subagent_session_manager;
mod workflow_session_manager;

use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use eos_sandbox_port::SandboxCommandApi;
use eos_tool::{BackgroundSessionCounts, BackgroundSessions, ToolError};
use eos_types::{
    AgentRunApi, AgentRunError, AgentRunId, AgentRunOutcome, CommandSessionId, SandboxId,
    SpawnAgentRequest, StartedWorkflow, WorkflowApi,
};

use self::command_session_manager::{CommandSessionManager, CommandSessionMonitor};
use self::subagent_session_manager::{SubagentSessionManager, SubagentSessionMonitor};
use self::workflow_session_manager::{
    WorkflowServiceCell, WorkflowSessionManager, WorkflowSessionMonitor,
};
use super::notification::BackgroundNotificationEmitter;
use crate::notifications::NotificationService;

type BackgroundTeardownFuture = Pin<Box<dyn Future<Output = BackgroundSessionCounts> + Send>>;
type BackgroundTeardownCallback = Arc<dyn Fn(String) -> BackgroundTeardownFuture + Send + Sync>;

/// Lifecycle status for one tracked background session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundSessionStatus {
    /// The session is still running.
    Running,
    /// The session completed normally.
    Completed,
    /// The session failed.
    Failed,
    /// The session was cancelled.
    Cancelled,
    /// The terminal tool outcome was already delivered to the model.
    Delivered,
}

impl BackgroundSessionStatus {
    /// Terminal precedence; higher status wins when cancel/completion events race.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Cancelled => 1,
            Self::Failed => 2,
            Self::Completed => 3,
            Self::Delivered => 4,
        }
    }
}

trait BackgroundSession {
    type Id: Eq + Hash + Clone + Send + Sync + 'static;

    fn id(&self) -> &Self::Id;
}

#[async_trait]
trait BackgroundSessionManager {
    type Session: BackgroundSession + Send + 'static;
    type Completion: Send + 'static;

    async fn insert(&self, session: Self::Session);
    async fn count(&self) -> usize;
    async fn push_notification_on_completion(&self, completion: Self::Completion);
    async fn cancel(&self, reason: &str);
}

/// Per-agent-run aggregate for background session accounting and lifecycle.
pub(super) struct BackgroundSessionRuntime {
    agent_run_id: AgentRunId,
    agent_run_service: Arc<dyn AgentRunApi>,
    subagent_session_manager: SubagentSessionManager,
    workflow_session_manager: WorkflowSessionManager,
    command_session_manager: CommandSessionManager,
    _subagent_monitor: SubagentSessionMonitor,
    _workflow_monitor: WorkflowSessionMonitor,
    _command_monitor: CommandSessionMonitor,
}

impl BackgroundSessionRuntime {
    pub(super) fn new(
        agent_run_id: AgentRunId,
        agent_run_service: Arc<dyn AgentRunApi>,
        command_service: Arc<dyn SandboxCommandApi>,
        completion_poll_interval: Duration,
        notifications: NotificationService,
        workflow_service: WorkflowServiceCell,
    ) -> Self {
        let notification = BackgroundNotificationEmitter::new(notifications);
        let subagent_session_manager = SubagentSessionManager::new(
            agent_run_id.clone(),
            agent_run_service.clone(),
            notification.clone(),
        );
        let workflow_session_manager = WorkflowSessionManager::new(
            agent_run_id.clone(),
            workflow_service,
            notification.clone(),
        );
        let command_session_manager =
            CommandSessionManager::new(agent_run_id.clone(), command_service, notification);
        let subagent_monitor = SubagentSessionMonitor::spawn(
            subagent_session_manager.clone(),
            completion_poll_interval,
        );
        let workflow_monitor = WorkflowSessionMonitor::spawn(
            workflow_session_manager.clone(),
            completion_poll_interval,
        );
        let command_monitor =
            CommandSessionMonitor::spawn(command_session_manager.clone(), completion_poll_interval);
        Self {
            agent_run_id,
            agent_run_service,
            subagent_session_manager,
            workflow_session_manager,
            command_session_manager,
            _subagent_monitor: subagent_monitor,
            _workflow_monitor: workflow_monitor,
            _command_monitor: command_monitor,
        }
    }

    pub(super) fn agent_run_id(&self) -> &AgentRunId {
        &self.agent_run_id
    }

    pub(super) fn subagent_session_manager(&self) -> &SubagentSessionManager {
        &self.subagent_session_manager
    }

    pub(super) fn agent_run_service(&self) -> &Arc<dyn AgentRunApi> {
        &self.agent_run_service
    }

    pub(super) fn workflow_session_manager(&self) -> &WorkflowSessionManager {
        &self.workflow_session_manager
    }

    pub(super) fn command_session_manager(&self) -> &CommandSessionManager {
        &self.command_session_manager
    }

    pub(super) async fn count(&self) -> BackgroundSessionCounts {
        let subagents = self.subagent_session_manager.count().await;
        let workflows = self.workflow_session_manager.count().await;
        let command_sessions = self.command_session_manager.count().await;
        BackgroundSessionCounts {
            total: subagents + workflows + command_sessions,
            subagents,
            workflows,
            command_sessions,
        }
    }
}

/// Cloneable aggregate for one agent run's background session runtime.
#[derive(Clone)]
pub struct BackgroundManagers {
    runtime: Arc<BackgroundSessionRuntime>,
}

impl std::fmt::Debug for BackgroundManagers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundManagers")
            .field("agent_run_id", self.runtime.agent_run_id())
            .finish_non_exhaustive()
    }
}

impl BackgroundManagers {
    #[must_use]
    pub fn new(
        agent_run_id: AgentRunId,
        agent_run_service: Arc<dyn AgentRunApi>,
        command_service: Arc<dyn SandboxCommandApi>,
        completion_poll_interval: Duration,
        notifications: NotificationService,
        workflow_service: &Arc<OnceLock<Arc<dyn WorkflowApi>>>,
    ) -> Self {
        Self {
            runtime: Arc::new(BackgroundSessionRuntime::new(
                agent_run_id,
                agent_run_service,
                command_service,
                completion_poll_interval,
                notifications,
                workflow_service.clone(),
            )),
        }
    }

    #[must_use]
    pub fn agent_run_id(&self) -> &AgentRunId {
        self.runtime.agent_run_id()
    }

    pub async fn teardown(&self, reason: &str) -> BackgroundSessionCounts {
        self.runtime.subagent_session_manager().cancel(reason).await;
        self.runtime.workflow_session_manager().cancel(reason).await;
        self.runtime.command_session_manager().cancel(reason).await;
        self.runtime.count().await
    }

    pub(crate) async fn count(&self) -> BackgroundSessionCounts {
        self.runtime.count().await
    }

    pub(crate) async fn flush_completions(&self) {
        for completion in self
            .runtime
            .subagent_session_manager()
            .poll_completions()
            .await
        {
            self.runtime
                .subagent_session_manager()
                .push_notification_on_completion(completion)
                .await;
        }
        for completion in self
            .runtime
            .workflow_session_manager()
            .poll_completions()
            .await
        {
            self.runtime
                .workflow_session_manager()
                .push_notification_on_completion(completion)
                .await;
        }
        for completion in self
            .runtime
            .command_session_manager()
            .poll_completions()
            .await
        {
            self.runtime
                .command_session_manager()
                .push_notification_on_completion(completion)
                .await;
        }
    }

    pub(crate) async fn cancel_all_subagents(&self, reason: &str) {
        self.runtime
            .subagent_session_manager()
            .cancel_all_background_sessions(reason)
            .await;
    }

    #[must_use]
    pub fn teardown_service(&self) -> BackgroundTeardownService {
        let background = self.clone();
        BackgroundTeardownService::new(move |reason| {
            let background = background.clone();
            async move { background.teardown(&reason).await }
        })
    }
}

#[async_trait]
impl BackgroundSessions for BackgroundManagers {
    async fn register_subagent(&self, run: AgentRunId) -> Result<(), ToolError> {
        self.runtime
            .subagent_session_manager()
            .register_background_session(&run)
            .await;
        Ok(())
    }

    async fn register_command(
        &self,
        id: CommandSessionId,
        sandbox: SandboxId,
    ) -> Result<(), ToolError> {
        self.runtime
            .command_session_manager()
            .register_background_session(&id, &sandbox)
            .await;
        Ok(())
    }

    async fn register_workflow(&self, started: StartedWorkflow) -> Result<(), ToolError> {
        self.runtime
            .workflow_session_manager()
            .register_background_session(&started)
            .await;
        Ok(())
    }

    async fn cancel_subagent(&self, run: AgentRunId, reason: &str) -> Result<bool, ToolError> {
        Ok(self
            .runtime
            .subagent_session_manager()
            .cancel_background_agent_run(&run, reason)
            .await)
    }
}

#[async_trait]
impl AgentRunApi for BackgroundManagers {
    async fn spawn_agent(
        &self,
        request: SpawnAgentRequest,
    ) -> Result<eos_types::AgentRunId, AgentRunError> {
        AgentRunApi::spawn_agent(self.runtime.agent_run_service().as_ref(), request).await
    }

    async fn wait_for_agent_outcome(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        AgentRunApi::wait_for_agent_outcome(self.runtime.agent_run_service().as_ref(), agent_run_id)
            .await
    }

    async fn poll_agent_run_outcome(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
        AgentRunApi::poll_agent_run_outcome(self.runtime.agent_run_service().as_ref(), agent_run_id)
            .await
    }

    async fn cancel_agent_run(
        &self,
        agent_run_id: &eos_types::AgentRunId,
        reason: &str,
    ) -> Result<(), AgentRunError> {
        AgentRunApi::cancel_agent_run(
            self.runtime.agent_run_service().as_ref(),
            agent_run_id,
            reason,
        )
        .await
    }
}

#[derive(Clone)]
pub struct BackgroundTeardownService {
    teardown: BackgroundTeardownCallback,
}

impl BackgroundTeardownService {
    #[must_use]
    pub fn new<Teardown, TeardownFuture>(teardown: Teardown) -> Self
    where
        Teardown: Fn(String) -> TeardownFuture + Send + Sync + 'static,
        TeardownFuture: Future<Output = BackgroundSessionCounts> + Send + 'static,
    {
        Self {
            teardown: Arc::new(move |reason| Box::pin(teardown(reason))),
        }
    }

    pub async fn teardown(&self, reason: &str) -> BackgroundSessionCounts {
        (self.teardown)(reason.to_owned()).await
    }
}

impl std::fmt::Debug for BackgroundTeardownService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundTeardownService")
            .finish_non_exhaustive()
    }
}

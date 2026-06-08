mod command_session_manager;
mod subagent_session_manager;
mod workflow_session_manager;

use std::hash::Hash;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use eos_agent_run::{AgentRunApi, AgentRunError, AgentRunOutcome, SpawnAgentRequest};
use eos_sandbox_port::SandboxCommandApi;
use eos_tools::ports::{
    BackgroundSessionCounts, CancelledSubagent, CommandSessionPort, StartedWorkflow,
    SubagentProgress, SubagentSessionPort, WorkflowSessionPort,
};
use eos_tools::WorkflowServicePort;
use eos_types::{AgentRunId, CommandSessionId, SandboxId, SubagentSessionId};

use self::command_session_manager::{CommandSessionManager, CommandSessionMonitor};
use self::subagent_session_manager::{SubagentSessionManager, SubagentSessionMonitor};
use self::workflow_session_manager::{
    WorkflowServiceCell, WorkflowSessionManager, WorkflowSessionMonitor,
};
use super::notification::BackgroundNotificationEmitter;
use crate::notifications::NotificationService;
use crate::query::{QueryContext, QueryExitReason};
use crate::runtime::{AgentRunControlFactory, AgentRunService};
use crate::EngineRunHandles;

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
    /// The terminal result was already delivered to the model.
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

    pub(super) async fn cancel(&self, reason: &str) -> BackgroundSessionCounts {
        self.subagent_session_manager.cancel(reason).await;
        self.workflow_session_manager.cancel(reason).await;
        self.command_session_manager.cancel(reason).await;
        self.count().await
    }
}

/// Cloneable port-facing service for one agent run's background session runtime.
#[derive(Clone)]
pub struct BackgroundSessionService {
    runtime: Arc<BackgroundSessionRuntime>,
}

impl std::fmt::Debug for BackgroundSessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSessionService")
            .field("agent_run_id", self.runtime.agent_run_id())
            .finish_non_exhaustive()
    }
}

impl BackgroundSessionService {
    #[must_use]
    pub fn new(
        agent_run_id: AgentRunId,
        handles: EngineRunHandles,
        command_service: Arc<dyn SandboxCommandApi>,
        completion_poll_interval: Duration,
        notifications: NotificationService,
        control_factory: AgentRunControlFactory,
        workflow_service: &Arc<OnceLock<Arc<dyn WorkflowServicePort>>>,
    ) -> Self {
        Self {
            runtime: Arc::new(BackgroundSessionRuntime::new(
                agent_run_id,
                Arc::new(AgentRunService::new(handles, control_factory)),
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

    pub(super) fn command_session_manager(&self) -> &CommandSessionManager {
        self.runtime.command_session_manager()
    }

    pub async fn running_background_tasks(&self) -> BackgroundSessionCounts {
        self.runtime.count().await
    }

    pub async fn cancel(&self, reason: &str) -> BackgroundSessionCounts {
        self.runtime.cancel(reason).await
    }

    pub async fn teardown(&self, reason: &str) -> BackgroundSessionCounts {
        self.runtime.subagent_session_manager().cancel(reason).await;
        self.runtime.workflow_session_manager().cancel(reason).await;
        self.runtime.command_session_manager().cancel(reason).await;
        self.runtime.count().await
    }
}

impl eos_tools::ports::Sealed for BackgroundSessionService {}

#[async_trait]
impl AgentRunApi for BackgroundSessionService {
    async fn spawn_agent(
        &self,
        request: SpawnAgentRequest,
    ) -> Result<eos_types::AgentRunId, AgentRunError> {
        self.runtime
            .agent_run_service()
            .spawn_agent(request)
            .await
    }

    async fn wait_for_agent_outcomes(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        self.runtime
            .agent_run_service()
            .wait_for_agent_outcomes(agent_run_id)
            .await
    }

    async fn poll_agent_run_outcome(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
        self.runtime
            .agent_run_service()
            .poll_agent_run_outcome(agent_run_id)
            .await
    }

    async fn cancel_agent_run(
        &self,
        agent_run_id: &eos_types::AgentRunId,
        reason: &str,
    ) -> Result<(), AgentRunError> {
        self.runtime
            .agent_run_service()
            .cancel_agent_run(agent_run_id, reason)
            .await
    }
}

#[async_trait]
impl SubagentSessionPort for BackgroundSessionService {
    async fn register_background_session(
        &self,
        agent_run_id: &AgentRunId,
        agent_name: &str,
    ) -> SubagentSessionId {
        self.runtime
            .subagent_session_manager()
            .register_background_session(agent_run_id, agent_name)
            .await
    }

    async fn subagent_session_snapshot(
        &self,
        subagent_session_id: &SubagentSessionId,
    ) -> Option<SubagentProgress> {
        self.runtime
            .subagent_session_manager()
            .subagent_session_snapshot(subagent_session_id)
            .await
    }

    async fn cancel_background_session(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> CancelledSubagent {
        self.runtime
            .subagent_session_manager()
            .cancel_background_session(subagent_session_id, reason)
            .await
    }

    async fn cancel_background_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> bool {
        self.runtime
            .subagent_session_manager()
            .cancel_agent_run(agent_run_id, reason)
            .await
    }

    async fn count_background_sessions(&self) -> usize {
        self.runtime
            .subagent_session_manager()
            .count_background_sessions()
            .await
    }

    async fn cancel_all_background_sessions(&self, reason: &str) {
        self.runtime
            .subagent_session_manager()
            .cancel_all_background_sessions(reason)
            .await;
    }

    async fn poll_complete_background_sessions(&self) -> usize {
        self.runtime
            .subagent_session_manager()
            .poll_complete_background_sessions()
            .await
    }
}

#[async_trait]
impl WorkflowSessionPort for BackgroundSessionService {
    async fn register_background_session(&self, workflow: &StartedWorkflow) {
        self.runtime
            .workflow_session_manager()
            .register_background_session(workflow)
            .await;
    }

    async fn count_background_sessions(&self) -> usize {
        self.runtime
            .workflow_session_manager()
            .count_background_sessions()
            .await
    }

    async fn cancel_all_background_sessions(&self, reason: &str) {
        self.runtime
            .workflow_session_manager()
            .cancel_all_background_sessions(reason)
            .await;
    }

    async fn poll_complete_background_sessions(&self) -> usize {
        self.runtime
            .workflow_session_manager()
            .poll_complete_background_sessions()
            .await
    }
}

#[async_trait]
impl CommandSessionPort for BackgroundSessionService {
    async fn register_background_session(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
    ) {
        self.command_session_manager()
            .register_background_session(command_session_id, sandbox_id)
            .await;
    }

    async fn count_background_sessions(&self) -> usize {
        self.command_session_manager()
            .count_background_sessions()
            .await
    }

    async fn cancel_all_background_sessions(&self, reason: &str) {
        self.command_session_manager()
            .cancel_all_background_sessions(reason)
            .await;
    }

    async fn poll_complete_background_sessions(&self) -> usize {
        self.command_session_manager()
            .poll_complete_background_sessions()
            .await
    }
}

#[async_trait]
pub trait BackgroundTeardownPort: Send + Sync {
    async fn teardown(&self, reason: &str) -> BackgroundSessionCounts;
}

#[async_trait]
impl BackgroundTeardownPort for BackgroundSessionService {
    async fn teardown(&self, reason: &str) -> BackgroundSessionCounts {
        BackgroundSessionService::teardown(self, reason).await
    }
}

/// Normal-exit background cleanup for one agent run.
pub(crate) struct BackgroundSessionFinalizer {
    background: Option<Arc<dyn BackgroundTeardownPort>>,
    armed: bool,
}

impl BackgroundSessionFinalizer {
    pub(crate) fn new(background: Option<Arc<dyn BackgroundTeardownPort>>) -> Self {
        Self {
            background,
            armed: true,
        }
    }

    pub(crate) async fn finalize(&mut self, ctx: &QueryContext, error: Option<&str>) {
        let Some(background) = &self.background else {
            self.disarm();
            return;
        };
        let reason = finalize_reason(ctx.exit_reason, error);
        background.teardown(&reason).await;
        self.disarm();
    }

    /// Disarm without running cleanup: the caller has handed background teardown
    /// to another owner, so neither `finalize` nor the `Drop` backstop should fire
    /// a second teardown.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BackgroundSessionFinalizer {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(background) = self.background.take() else {
            return;
        };
        let reason = "engine run dropped before background finalization".to_owned();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "engine run dropped outside a Tokio runtime; background cleanup could not be spawned"
            );
            return;
        };
        handle.spawn(async move {
            background.teardown(&reason).await;
        });
    }
}

fn finalize_reason(exit_reason: Option<QueryExitReason>, error: Option<&str>) -> String {
    match (exit_reason, error) {
        (_, Some(error)) => format!("engine run failed: {error}"),
        (Some(QueryExitReason::TerminalNotSubmitted), None) => {
            "parent agent exited without submitting a terminal tool".to_owned()
        }
        (Some(QueryExitReason::ToolStop), None) => "parent agent submitted its terminal".to_owned(),
        (None, None) => "parent agent exited".to_owned(),
    }
}

#[cfg(test)]
mod finalizer_tests {
    #![allow(clippy::expect_used)]

    use async_trait::async_trait;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    use super::*;

    #[derive(Debug)]
    struct RecordingBackground {
        tx: mpsc::UnboundedSender<String>,
    }

    #[async_trait]
    impl BackgroundTeardownPort for RecordingBackground {
        async fn teardown(&self, reason: &str) -> BackgroundSessionCounts {
            self.tx.send(reason.to_owned()).expect("send cleanup");
            BackgroundSessionCounts {
                total: 0,
                subagents: 0,
                workflows: 0,
                command_sessions: 0,
            }
        }
    }

    #[tokio::test]
    async fn drop_spawns_background_cleanup_when_still_armed() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let background = Arc::new(RecordingBackground { tx });

        {
            let _finalizer = BackgroundSessionFinalizer::new(Some(background));
        }

        let reason = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("cleanup spawned")
            .expect("cleanup message");
        assert_eq!(reason, "engine run dropped before background finalization");
    }
}

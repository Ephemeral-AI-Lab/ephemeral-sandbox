use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use eos_sandbox_port::SandboxTransport;
use eos_tools::ports::{
    BackgroundSupervisorPort, CancelledSubagent, CommandSessionSupervisorPort,
    RunningBackgroundTasks, SpawnedSubagent, StartedWorkflowHandle, SubagentLaunch,
    SubagentProgress, SubagentProgressSnapshot as ToolSubagentProgressSnapshot,
    SubagentSessionStatus,
};
use eos_tools::{ExecutionMetadata, ToolError, WorkflowControlPort};
use eos_types::{AgentRunId, CommandSessionId, SandboxId, SubagentSessionId, WorkflowSessionId};
use serde_json::Value;

use super::notification::BackgroundNotificationEmitter;
use super::session_managers::command::{CommandSessionManager, CommandSessionMonitor};
use super::session_managers::subagent::{SubagentSessionManager, SubagentSessionMonitor};
use super::session_managers::workflow::{
    WorkflowControlCell, WorkflowSessionManager, WorkflowSessionMonitor,
};
use super::session_managers::{BackgroundSessionManager, BackgroundSessionMonitor};
use crate::notifications::NotificationService;
use crate::query::{QueryContext, QueryExitReason};
use crate::runtime::AgentRunControlFactory;
use crate::EngineRunHandles;

/// Per-agent-run aggregate for background session accounting and lifecycle.
pub(super) struct BackgroundSessionRuntime {
    agent_run_id: AgentRunId,
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
        handles: EngineRunHandles,
        command_port: Arc<dyn SandboxTransport>,
        completion_poll_interval: Duration,
        notifications: NotificationService,
        control_factory: AgentRunControlFactory,
        workflow_port: WorkflowControlCell,
    ) -> Self {
        let notification = BackgroundNotificationEmitter::new(notifications);
        let subagent_session_manager =
            SubagentSessionManager::new(handles, control_factory, notification.clone());
        let workflow_session_manager =
            WorkflowSessionManager::new(workflow_port, notification.clone());
        let command_session_manager =
            CommandSessionManager::new(agent_run_id.clone(), command_port, notification);
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

    pub(super) fn workflow_session_manager(&self) -> &WorkflowSessionManager {
        &self.workflow_session_manager
    }

    pub(super) fn command_session_manager(&self) -> &CommandSessionManager {
        &self.command_session_manager
    }

    pub(super) async fn count(&self) -> RunningBackgroundTasks {
        let subagents = self.subagent_session_manager.count().await;
        let workflows = self.workflow_session_manager.count().await;
        let command_sessions = self.command_session_manager.count().await;
        RunningBackgroundTasks {
            total: subagents + workflows + command_sessions,
            subagents,
            workflows,
            command_sessions,
        }
    }

    pub(super) async fn cancel(&self, reason: &str) -> RunningBackgroundTasks {
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
        command_port: Arc<dyn SandboxTransport>,
        completion_poll_interval: Duration,
        notifications: NotificationService,
        control_factory: AgentRunControlFactory,
        workflow_port: &Arc<OnceLock<Arc<dyn WorkflowControlPort>>>,
    ) -> Self {
        Self {
            runtime: Arc::new(BackgroundSessionRuntime::new(
                agent_run_id,
                handles,
                command_port,
                completion_poll_interval,
                notifications,
                control_factory,
                workflow_port.clone(),
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

    pub async fn running_background_tasks(&self) -> RunningBackgroundTasks {
        self.runtime.count().await
    }

    pub async fn cancel(&self, reason: &str) -> RunningBackgroundTasks {
        self.runtime.cancel(reason).await
    }

    pub async fn cancel_subagents(&self, reason: &str) -> RunningBackgroundTasks {
        self.runtime.subagent_session_manager().cancel(reason).await;
        self.runtime.count().await
    }

    pub async fn teardown(
        &self,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> RunningBackgroundTasks {
        self.runtime.subagent_session_manager().cancel(reason).await;
        self.runtime
            .workflow_session_manager()
            .cancel_with_port(workflow_control, reason)
            .await;
        self.runtime.command_session_manager().cancel(reason).await;
        self.runtime.count().await
    }
}

impl eos_tools::ports::Sealed for BackgroundSessionService {}

#[async_trait]
impl BackgroundSupervisorPort for BackgroundSessionService {
    async fn spawn(
        &self,
        ctx: &ExecutionMetadata,
        launch: SubagentLaunch,
    ) -> Result<SpawnedSubagent, ToolError> {
        self.runtime
            .subagent_session_manager()
            .spawn(ctx, launch)
            .await
    }

    async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
        _last_n_messages: u8,
    ) -> Result<SubagentProgress, ToolError> {
        let Some(snapshot) = self
            .runtime
            .subagent_session_manager()
            .progress_snapshot(subagent_session_id)
            .await
        else {
            return Ok(SubagentProgress::Missing {
                subagent_session_id: subagent_session_id.clone(),
            });
        };
        Ok(SubagentProgress::Found(ToolSubagentProgressSnapshot {
            subagent_session_id: subagent_session_id.clone(),
            status: subagent_status(snapshot.status),
            agent_name: snapshot.agent_name,
            result: snapshot.result,
        }))
    }

    async fn cancel(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> Result<CancelledSubagent, ToolError> {
        let cancelled = self
            .runtime
            .subagent_session_manager()
            .cancel_one(subagent_session_id, reason)
            .await;
        if !cancelled {
            return Ok(CancelledSubagent::MissingOrSettled {
                subagent_session_id: subagent_session_id.clone(),
            });
        }
        Ok(CancelledSubagent::Cancelled {
            subagent_session_id: subagent_session_id.clone(),
            reason: reason.to_owned(),
        })
    }

    async fn running_background_tasks(&self) -> RunningBackgroundTasks {
        BackgroundSessionService::running_background_tasks(self).await
    }

    async fn cancel_subagents(&self) -> RunningBackgroundTasks {
        BackgroundSessionService::cancel_subagents(self, "parent submitted its terminal").await
    }

    async fn register_workflow(&self, workflow: &StartedWorkflowHandle) {
        self.runtime
            .workflow_session_manager()
            .register(workflow)
            .await;
    }

    async fn cancel_workflow_record(
        &self,
        workflow_task_id: &WorkflowSessionId,
        _reason: &str,
    ) -> bool {
        self.runtime
            .workflow_session_manager()
            .cancel_record(workflow_task_id)
            .await
    }

    async fn teardown(
        &self,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> RunningBackgroundTasks {
        BackgroundSessionService::teardown(self, workflow_control, reason).await
    }
}

const fn subagent_status(
    status: super::session_managers::BackgroundSessionStatus,
) -> SubagentSessionStatus {
    match status {
        super::session_managers::BackgroundSessionStatus::Running => SubagentSessionStatus::Running,
        super::session_managers::BackgroundSessionStatus::Completed => {
            SubagentSessionStatus::Completed
        }
        super::session_managers::BackgroundSessionStatus::Failed => SubagentSessionStatus::Failed,
        super::session_managers::BackgroundSessionStatus::Cancelled => {
            SubagentSessionStatus::Cancelled
        }
        super::session_managers::BackgroundSessionStatus::Delivered => {
            SubagentSessionStatus::Delivered
        }
    }
}

#[async_trait]
impl CommandSessionSupervisorPort for BackgroundSessionService {
    async fn register(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
        command: &str,
    ) {
        self.command_session_manager()
            .register(command_session_id, sandbox_id, command)
            .await;
    }

    async fn command_session_result(&self, command_session_id: &CommandSessionId) -> Option<Value> {
        self.command_session_manager()
            .command_session_result(command_session_id)
            .await
    }

    async fn mark_command_session_reported(
        &self,
        command_session_id: &CommandSessionId,
        result: Value,
    ) {
        self.command_session_manager()
            .mark_command_session_reported(command_session_id, result)
            .await;
    }

    async fn command_session_already_reported(
        &self,
        command_session_id: &CommandSessionId,
    ) -> bool {
        self.command_session_manager()
            .command_session_already_reported(command_session_id)
            .await
    }
}

/// Normal-exit background cleanup for one agent run.
pub(crate) struct BackgroundRunFinalizer {
    background: Option<Arc<dyn BackgroundSupervisorPort>>,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    armed: bool,
}

impl BackgroundRunFinalizer {
    pub(crate) fn new(
        background: Option<Arc<dyn BackgroundSupervisorPort>>,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    ) -> Self {
        Self {
            background,
            workflow_control,
            armed: true,
        }
    }

    pub(crate) async fn finalize(&mut self, ctx: &QueryContext, error: Option<&str>) {
        let Some(background) = &self.background else {
            self.disarm();
            return;
        };
        let reason = finalize_reason(ctx.exit_reason, error);
        background
            .teardown(self.workflow_control.clone(), &reason)
            .await;
        self.disarm();
    }

    /// Disarm without running cleanup: the caller has handed background teardown
    /// to another owner, so neither `finalize` nor the `Drop` backstop should fire
    /// a second teardown.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BackgroundRunFinalizer {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(background) = self.background.take() else {
            return;
        };
        let workflow_control = self.workflow_control.take();
        let reason = "engine run dropped before background finalization".to_owned();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "engine run dropped outside a Tokio runtime; background cleanup could not be spawned"
            );
            return;
        };
        handle.spawn(async move {
            background.teardown(workflow_control, &reason).await;
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
    use eos_tools::{
        CancelledSubagent, RunningBackgroundTasks, SpawnedSubagent, StartedSubagent,
        StartedWorkflowHandle, SubagentProgress, ToolError,
    };
    use eos_types::{SubagentSessionId, WorkflowSessionId};
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    use super::*;

    #[derive(Debug)]
    struct RecordingBackground {
        tx: mpsc::UnboundedSender<String>,
    }

    impl eos_tools::ports::Sealed for RecordingBackground {}

    fn empty_report() -> RunningBackgroundTasks {
        RunningBackgroundTasks {
            total: 0,
            subagents: 0,
            workflows: 0,
            command_sessions: 0,
        }
    }

    #[async_trait]
    impl BackgroundSupervisorPort for RecordingBackground {
        async fn spawn(
            &self,
            _ctx: &eos_tools::ExecutionMetadata,
            _launch: SubagentLaunch,
        ) -> Result<SpawnedSubagent, ToolError> {
            Ok(SpawnedSubagent::Launched(StartedSubagent {
                subagent_session_id: "subagent_1".parse().expect("subagent id"),
            }))
        }

        async fn progress(
            &self,
            subagent_session_id: &SubagentSessionId,
            _last_n_messages: u8,
        ) -> Result<SubagentProgress, ToolError> {
            Ok(SubagentProgress::Missing {
                subagent_session_id: subagent_session_id.clone(),
            })
        }

        async fn cancel(
            &self,
            subagent_session_id: &SubagentSessionId,
            _reason: &str,
        ) -> Result<CancelledSubagent, ToolError> {
            Ok(CancelledSubagent::MissingOrSettled {
                subagent_session_id: subagent_session_id.clone(),
            })
        }

        async fn running_background_tasks(&self) -> RunningBackgroundTasks {
            empty_report()
        }

        async fn cancel_subagents(&self) -> RunningBackgroundTasks {
            empty_report()
        }

        async fn register_workflow(&self, _workflow: &StartedWorkflowHandle) {}

        async fn cancel_workflow_record(
            &self,
            _workflow_task_id: &WorkflowSessionId,
            _reason: &str,
        ) -> bool {
            false
        }

        async fn teardown(
            &self,
            _workflow_control: Option<Arc<dyn WorkflowControlPort>>,
            reason: &str,
        ) -> RunningBackgroundTasks {
            self.tx.send(reason.to_owned()).expect("send cleanup");
            empty_report()
        }
    }

    #[tokio::test]
    async fn drop_spawns_background_cleanup_when_still_armed() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let background = Arc::new(RecordingBackground { tx });

        {
            let _finalizer = BackgroundRunFinalizer::new(Some(background), None);
        }

        let reason = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("cleanup spawned")
            .expect("cleanup message");
        assert_eq!(reason, "engine run dropped before background finalization");
    }
}

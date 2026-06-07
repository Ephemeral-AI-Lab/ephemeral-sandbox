//! [`BackgroundSupervisorHandle`] — the async wrapper over
//! [`BackgroundTaskSupervisor`] that the subagent driver and command-session
//! tools call through. It is the real
//! [`BackgroundSupervisorPort`](eos_tools::ports::BackgroundSupervisorPort)
//! (impl in `subagent.rs`) and
//! [`CommandSessionSupervisorPort`](eos_tools::ports::CommandSessionSupervisorPort)
//! (impl in `command_session.rs`), holds the [`EngineRunHandles`] the subagent
//! driver needs, and owns the parent-exit cleanup path.

use std::sync::Arc;

use eos_sandbox_port::{CommandSessionCancelRequest, SandboxRequestBase, SandboxTransport};
use eos_tools::ports::Sealed;
use eos_tools::{BackgroundInflightReport, WorkflowControlPort};
use eos_types::AgentRunId;
use tokio::sync::Mutex;

use super::supervisor::{BackgroundTaskSupervisor, CommandSessionCancelTarget};
use crate::runtime::AgentRunControlFactory;
use crate::EngineRunHandles;

/// The run dependencies the subagent driver needs, threaded in at the
/// composition root: the engine run handles (registry + stores + client +
/// workspace root) and a clone of the request-scoped [`AgentRunControlFactory`]
/// so `spawn` can mint each subagent its **own** ephemeral `AgentRunControl`
/// (own notifier, supervisor, heartbeat, and command sessions) — spec §8.2/§11.3.
#[derive(Clone)]
pub struct BackgroundSupervisorHandle {
    inner: Arc<Mutex<BackgroundTaskSupervisor>>,
    pub(super) handles: EngineRunHandles,
    transport: Arc<dyn SandboxTransport>,
    control_factory: AgentRunControlFactory,
}

impl std::fmt::Debug for BackgroundSupervisorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSupervisorHandle")
            .finish_non_exhaustive()
    }
}

impl BackgroundSupervisorHandle {
    /// Create the per-agent-run supervisor with the run handles the subagent
    /// driver needs and the control factory used to mint per-subagent controls.
    /// The ledger starts empty.
    #[must_use]
    pub fn new(
        handles: EngineRunHandles,
        transport: Arc<dyn SandboxTransport>,
        control_factory: AgentRunControlFactory,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackgroundTaskSupervisor::new())),
            handles,
            transport,
            control_factory,
        }
    }

    /// Access the shared supervisor state for the heartbeat and runtime adapters.
    #[must_use]
    pub fn inner(&self) -> Arc<Mutex<BackgroundTaskSupervisor>> {
        self.inner.clone()
    }

    /// The control factory used to mint each subagent its own ephemeral control.
    pub(super) fn control_factory(&self) -> &AgentRunControlFactory {
        &self.control_factory
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
        if let Err(err) = eos_sandbox_port::cancel_command_session(
            &*self.transport,
            &command.sandbox_id,
            &request,
        )
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use eos_agent_def::AgentRegistry;
    use eos_audit::NoopAuditSink;
    use eos_llm_client::{LlmClient, LlmRequest, LlmStream, ProviderError};
    use eos_sandbox_port::{DaemonOp, SandboxPortError};
    use eos_skills::SkillRegistry;
    use eos_state::{
        AgentRun, AgentRunStore, CoreError, Sealed as StateSealed, TaskId, UtcDateTime,
    };
    use eos_tools::{
        OutstandingWorkflow, SandboxToolService, SkillToolService, StartedWorkflowHandle,
        ToolConfigSet, ToolError,
    };
    use eos_types::{JsonObject, SandboxId, WorkflowId, WorkflowSessionId};
    use serde_json::json;

    use crate::{BackgroundSupervisorFactory, ForegroundExecutorFactory};

    use super::*;

    fn test_control_factory(transport: Arc<dyn SandboxTransport>) -> AgentRunControlFactory {
        AgentRunControlFactory::new(
            ForegroundExecutorFactory,
            BackgroundSupervisorFactory::new(
                handles(transport.clone()),
                transport,
                std::time::Duration::from_secs(3600),
            ),
        )
    }

    #[derive(Debug)]
    struct NoopLlmClient;

    #[async_trait]
    impl LlmClient for NoopLlmClient {
        async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[derive(Debug, Default)]
    struct NoopAgentRunStore;

    impl StateSealed for NoopAgentRunStore {}

    #[async_trait]
    impl AgentRunStore for NoopAgentRunStore {
        async fn create_run(
            &self,
            agent_run_id: &AgentRunId,
            task_id: &TaskId,
            agent_name: &str,
            initial_messages: Option<&[JsonObject]>,
        ) -> Result<AgentRun, CoreError> {
            Ok(AgentRun {
                id: agent_run_id.clone(),
                task_id: task_id.clone(),
                initial_messages: initial_messages.map(<[_]>::to_vec),
                agent_name: agent_name.to_owned(),
                message_history: None,
                terminal_tool_result: None,
                token_count: 0,
                error: None,
                created_at: UtcDateTime::now(),
                finished_at: None,
            })
        }

        async fn finish_run(
            &self,
            _agent_run_id: &AgentRunId,
            _message_history: Option<&[JsonObject]>,
            _terminal_tool_result: Option<&JsonObject>,
            _token_count: i64,
            _error: Option<&str>,
        ) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get(&self, _agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get_for_task(&self, _task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }
    }

    #[derive(Debug, Default)]
    struct RecordingTransport {
        calls: StdMutex<Vec<(SandboxId, DaemonOp, JsonObject)>>,
    }

    impl RecordingTransport {
        fn calls(&self) -> Vec<(SandboxId, DaemonOp, JsonObject)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait]
    impl SandboxTransport for RecordingTransport {
        async fn call(
            &self,
            sandbox_id: &SandboxId,
            op: DaemonOp,
            payload: JsonObject,
            _timeout_s: u32,
        ) -> Result<JsonObject, SandboxPortError> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((sandbox_id.clone(), op, payload));
            Ok(json!({
                "success": true,
                "status": "cancelled",
                "exit_code": null,
                "output": {"stdout": "", "stderr": ""}
            })
            .as_object()
            .expect("object")
            .clone())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingWorkflowControl {
        cancels: StdMutex<Vec<(WorkflowSessionId, String)>>,
    }

    impl RecordingWorkflowControl {
        fn cancels(&self) -> Vec<(WorkflowSessionId, String)> {
            self.cancels.lock().expect("workflow lock").clone()
        }
    }

    impl eos_tools::ports::Sealed for RecordingWorkflowControl {}

    #[async_trait]
    impl WorkflowControlPort for RecordingWorkflowControl {
        async fn start(
            &self,
            _parent_task_id: &TaskId,
            _agent_run_id: &AgentRunId,
            _workflow_goal: &str,
        ) -> Result<StartedWorkflowHandle, ToolError> {
            Ok(StartedWorkflowHandle {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: "wf_started".parse().expect("workflow handle"),
            })
        }

        async fn status(
            &self,
            _workflow_id: &WorkflowId,
            _workflow_task_id: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            Ok("running".to_owned())
        }

        async fn cancel(
            &self,
            workflow_task_id: &WorkflowSessionId,
            reason: &str,
        ) -> Result<String, ToolError> {
            self.cancels
                .lock()
                .expect("workflow lock")
                .push((workflow_task_id.clone(), reason.to_owned()));
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

    fn handles(transport: Arc<dyn SandboxTransport>) -> EngineRunHandles {
        EngineRunHandles {
            agent_run_store: Arc::new(NoopAgentRunStore),
            llm_client: Arc::new(NoopLlmClient),
            event_source_factory: None,
            agent_registry: Arc::new(Vec::new().into_iter().collect::<AgentRegistry>()),
            tool_config: Arc::new(
                ToolConfigSet::load_from_dir(&eos_testkit::test_tools_root()).expect("tool config"),
            ),
            sandbox_service: SandboxToolService::new(transport),
            root_submission: None,
            skill_service: SkillToolService::new(Arc::new(SkillRegistry::new())),
            tool_registry_extender: None,
            audit: Arc::new(NoopAuditSink),
            message_records: None,
            workspace_root: "/tmp".to_owned(),
        }
    }

    #[tokio::test]
    async fn parent_exit_cancels_workflows_and_command_sessions() {
        let transport = Arc::new(RecordingTransport::default());
        let handle = BackgroundSupervisorHandle::new(
            handles(transport.clone()),
            transport.clone(),
            test_control_factory(transport.clone()),
        );
        let agent_run_id: AgentRunId = "agent-a".parse().expect("agent run id");
        let workflow = StartedWorkflowHandle {
            workflow_id: WorkflowId::new_v4(),
            workflow_task_id: "wf_1".parse().expect("workflow handle"),
        };
        {
            let inner = handle.inner();
            let mut guard = inner.lock().await;
            guard.register_workflow(&agent_run_id, &workflow);
            guard.register_command_session(
                &"cmd_1".parse().expect("command id"),
                &"sandbox-a".parse().expect("sandbox id"),
                &agent_run_id,
                "cargo test",
            );
        }
        let workflow_control = Arc::new(RecordingWorkflowControl::default());

        let report = handle
            .cancel_for_parent_exit(
                Some(&agent_run_id),
                Some(workflow_control.clone()),
                "parent submitted its terminal",
            )
            .await;

        assert_eq!(report.total, 0);
        assert_eq!(workflow_control.cancels().len(), 1);
        assert_eq!(workflow_control.cancels()[0].0, workflow.workflow_task_id);
        let calls = transport.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, DaemonOp::CommandCancel);
        assert_eq!(calls[0].2["command_session_id"], json!("cmd_1"));
        assert_eq!(calls[0].2["caller_id"], json!("agent-a"));
    }

    #[tokio::test]
    async fn parent_exit_settles_workflow_without_workflow_control() {
        let transport = Arc::new(RecordingTransport::default());
        let handle = BackgroundSupervisorHandle::new(
            handles(transport.clone()),
            transport.clone(),
            test_control_factory(transport),
        );
        let agent_run_id: AgentRunId = "agent-a".parse().expect("agent run id");
        let workflow = StartedWorkflowHandle {
            workflow_id: WorkflowId::new_v4(),
            workflow_task_id: "wf_2".parse().expect("workflow handle"),
        };
        handle
            .inner()
            .lock()
            .await
            .register_workflow(&agent_run_id, &workflow);

        let report = handle
            .cancel_for_parent_exit(Some(&agent_run_id), None, "parent exited")
            .await;

        assert_eq!(report.workflow, 0);
        assert_eq!(report.total, 0);
    }
}

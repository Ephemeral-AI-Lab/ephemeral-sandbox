//! Launcher-backed agent-run lifecycle service.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{
    AgentLoopLauncher, AgentRegistry, AgentRunApi, AgentRunError, AgentRunId, AgentRunOutcome,
    AgentRunStore, CreatedTaskAgentRun, SpawnAgentRequest, TaskAgentRunStore,
};

use crate::active_agent_runs::ActiveAgentRunRegistry;
use crate::{cancellation, completion, spawn};

type RuntimeStateRecorder = Arc<
    dyn Fn(&SpawnAgentRequest, &CreatedTaskAgentRun) -> Result<(), AgentRunError> + Send + Sync,
>;
type RuntimeStateRemover = Arc<dyn Fn(&AgentRunId) + Send + Sync>;

/// Runtime-only state store for mutable execution facts outside durable run rows.
pub trait AgentRuntimeStateStore: Send + Sync {
    /// Record state needed by runtime metadata and tool execution after spawn.
    fn record_spawn_request(
        &self,
        request: &SpawnAgentRequest,
        created_run: &CreatedTaskAgentRun,
    ) -> Result<(), AgentRunError>;

    /// Remove runtime state after a run reaches a terminal outcome.
    fn remove_runtime_state(&self, agent_run_id: &AgentRunId);
}

struct RuntimeStateHooks {
    record: RuntimeStateRecorder,
    remove: RuntimeStateRemover,
}

impl AgentRuntimeStateStore for RuntimeStateHooks {
    fn record_spawn_request(
        &self,
        request: &SpawnAgentRequest,
        created_run: &CreatedTaskAgentRun,
    ) -> Result<(), AgentRunError> {
        (self.record)(request, created_run)
    }

    fn remove_runtime_state(&self, agent_run_id: &AgentRunId) {
        (self.remove)(agent_run_id);
    }
}

/// Agent-run lifecycle service.
#[derive(Clone)]
pub struct AgentRunService {
    pub(crate) agent_registry: Arc<AgentRegistry>,
    pub(crate) loop_launcher: Arc<dyn AgentLoopLauncher>,
    pub(crate) agent_run_store: Arc<dyn AgentRunStore>,
    pub(crate) task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    pub(crate) active_agent_runs: ActiveAgentRunRegistry,
    pub(crate) runtime_state: Option<Arc<dyn AgentRuntimeStateStore>>,
}

impl std::fmt::Debug for AgentRunService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunService").finish_non_exhaustive()
    }
}

impl AgentRunService {
    /// Build a runner service from injected trait contracts.
    #[must_use]
    pub fn new(
        agent_registry: Arc<AgentRegistry>,
        loop_launcher: Arc<dyn AgentLoopLauncher>,
        agent_run_store: Arc<dyn AgentRunStore>,
        task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    ) -> Self {
        Self {
            agent_registry,
            loop_launcher,
            agent_run_store,
            task_agent_run_store,
            active_agent_runs: ActiveAgentRunRegistry::new(),
            runtime_state: None,
        }
    }

    /// Attach a runtime-only state store used by the production composition layer.
    #[must_use]
    pub fn with_runtime_state(mut self, runtime_state: Arc<dyn AgentRuntimeStateStore>) -> Self {
        self.runtime_state = Some(runtime_state);
        self
    }

    /// Attach runtime-only state hooks used by the production composition layer.
    ///
    /// The runner still owns agent-run lifecycle state; these hooks only record
    /// and remove mutable execution facts such as workspace/isolation metadata.
    #[must_use]
    pub fn with_runtime_state_hooks<Record, Remove>(self, record: Record, remove: Remove) -> Self
    where
        Record: Fn(&SpawnAgentRequest, &CreatedTaskAgentRun) -> Result<(), AgentRunError>
            + Send
            + Sync
            + 'static,
        Remove: Fn(&AgentRunId) + Send + Sync + 'static,
    {
        self.with_runtime_state(Arc::new(RuntimeStateHooks {
            record: Arc::new(record),
            remove: Arc::new(remove),
        }))
    }
}

#[async_trait]
impl AgentRunApi for AgentRunService {
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> Result<AgentRunId, AgentRunError> {
        spawn::spawn_agent(self, request).await
    }

    async fn wait_for_agent_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        completion::wait_for_agent_outcome(self, agent_run_id).await
    }

    async fn poll_agent_run_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
        completion::poll_agent_run_outcome(self, agent_run_id).await
    }

    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), AgentRunError> {
        cancellation::cancel_agent_run(self, agent_run_id, reason).await
    }
}

#[cfg(test)]
use crate::spawn::expected_agent_type;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex as StdMutex, MutexGuard};
    use tokio::sync::oneshot;
    use tokio::time::{timeout, Duration};

    use eos_types::{
        format_record_dir, root_task_id, AgentDefinition, AgentLoopCancellation,
        AgentLoopCompletion, AgentLoopLauncher, AgentLoopMessage, AgentLoopOutcome,
        AgentLoopOutcomeKind, AgentName, AgentRegistryBuilder, AgentRun, AgentRunApi,
        AgentRunRecordIndex, AgentRunRecordTarget, AgentRunStatus, AgentRunStore, AgentType,
        ContentBlock, CoreError, CreatedTaskAgentRun, JsonObject, Message, ParentAgentRunAnchor,
        ParentedAgentRunKind, ParentedRun, RequestId, RunningRequestAgentRun, SpawnAgentRequest,
        SpawnAgentTarget, StartAgentLoopRequest, StartedAgentLoop, TaskAgentRunKind,
        TaskAgentRunStore, TaskExecutionIndex, TaskId, TaskRole, TaskRun, TaskStatus, ToolUseId,
        UtcDateTime, WorkflowCoordinates, WorkflowNodeId,
    };

    #[test]
    fn task_agent_run_kind_declares_required_agent_type() {
        let parent_agent_run_id = AgentRunId::new_v4();
        assert_eq!(
            expected_agent_type(&TaskAgentRunKind::Root),
            AgentType::Agent
        );
        assert_eq!(
            expected_agent_type(&TaskAgentRunKind::Parented {
                parent_agent_run_id: parent_agent_run_id.clone(),
                kind: ParentedAgentRunKind::Subagent,
            }),
            AgentType::Subagent
        );
        assert_eq!(
            expected_agent_type(&TaskAgentRunKind::Parented {
                parent_agent_run_id,
                kind: ParentedAgentRunKind::Advisor,
            }),
            AgentType::Advisor
        );
    }

    #[tokio::test]
    async fn engine_completion_finalizes_once_and_publishes_waiters() {
        let harness = ServiceHarness::new();
        let run_id = harness
            .service
            .spawn_agent(root_spawn_request())
            .await
            .expect("spawn succeeds");
        let waiter = {
            let service = harness.service.clone();
            let run_id = run_id.clone();
            tokio::spawn(async move { service.wait_for_agent_outcome(&run_id).await })
        };

        harness.launcher.complete(successful_loop_outcome());

        let outcome = timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter completes")
            .expect("waiter task joins")
            .expect("waiter returns outcome");
        assert_eq!(outcome.status, AgentRunStatus::Completed);
        assert_eq!(harness.agent_run_store.finish_count(), 1);
        assert_eq!(harness.task_agent_run_store.finish_count(), 1);
        assert_eq!(
            harness
                .service
                .poll_agent_run_outcome(&run_id)
                .await
                .expect("poll succeeds")
                .expect("outcome is persisted")
                .status,
            AgentRunStatus::Completed
        );
    }

    #[tokio::test]
    async fn cancellation_before_engine_completion_finalizes_once() {
        let harness = ServiceHarness::new();
        let run_id = harness
            .service
            .spawn_agent(root_spawn_request())
            .await
            .expect("spawn succeeds");
        let waiter = {
            let service = harness.service.clone();
            let run_id = run_id.clone();
            tokio::spawn(async move { service.wait_for_agent_outcome(&run_id).await })
        };

        harness
            .service
            .cancel_agent_run(&run_id, "caller cancelled")
            .await
            .expect("cancel succeeds");

        let outcome = timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter completes")
            .expect("waiter task joins")
            .expect("waiter returns outcome");
        assert_eq!(outcome.status, AgentRunStatus::Cancelled);
        assert_eq!(
            harness.launcher.cancellation_reason(),
            Some("caller cancelled".to_owned())
        );
        assert_eq!(harness.agent_run_store.finish_count(), 1);
        assert_eq!(harness.task_agent_run_store.finish_count(), 1);

        harness.launcher.complete(successful_loop_outcome());
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(harness.agent_run_store.finish_count(), 1);
        assert_eq!(harness.task_agent_run_store.finish_count(), 1);
    }

    struct ServiceHarness {
        service: AgentRunService,
        launcher: Arc<ControlledLauncher>,
        agent_run_store: Arc<FakeAgentRunStore>,
        task_agent_run_store: Arc<FakeTaskAgentRunStore>,
    }

    impl ServiceHarness {
        fn new() -> Self {
            let mut registry = AgentRegistryBuilder::new();
            registry.add(root_agent_definition());
            let launcher = Arc::new(ControlledLauncher::default());
            let agent_run_store = Arc::new(FakeAgentRunStore::default());
            let task_agent_run_store = Arc::new(FakeTaskAgentRunStore::default());
            let service = AgentRunService::new(
                Arc::new(registry.build()),
                launcher.clone(),
                agent_run_store.clone(),
                task_agent_run_store.clone(),
            );
            Self {
                service,
                launcher,
                agent_run_store,
                task_agent_run_store,
            }
        }
    }

    fn root_agent_definition() -> AgentDefinition {
        AgentDefinition {
            name: AgentName::new("root").expect("valid agent name"),
            description: "root test agent".to_owned(),
            system_prompt: Some("system".to_owned()),
            model: Some("test-model".to_owned()),
            tool_call_limit: NonZeroU32::new(4).expect("non-zero"),
            agent_type: AgentType::Agent,
            allowed_tools: Vec::new(),
            terminals: Vec::new(),
            notification_triggers: Vec::new(),
            skill: None,
            context_recipe: None,
        }
    }

    fn root_spawn_request() -> SpawnAgentRequest {
        SpawnAgentRequest {
            agent_name: AgentName::new("root").expect("valid agent name"),
            initial_messages: vec![Message::from_user_text("start")],
            target: SpawnAgentTarget::Root {
                request_id: RequestId::new_v4(),
            },
            tool_use_id: None,
            sandbox_id: None,
            workspace_root: "/workspace".to_owned(),
            is_isolated_workspace_mode: false,
        }
    }

    fn successful_loop_outcome() -> AgentLoopOutcome {
        let mut payload = JsonObject::new();
        payload.insert("summary".to_owned(), serde_json::json!("done"));
        AgentLoopOutcome {
            kind: AgentLoopOutcomeKind::TerminalToolSubmitted {
                submission_payload: payload,
            },
            final_conversation_messages: vec![
                AgentLoopMessage::UserMessage(Message::from_user_text("start")),
                AgentLoopMessage::AssistantMessage(Message {
                    role: eos_types::MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "done".to_owned(),
                    }],
                }),
            ],
            total_token_count: Some(12),
        }
    }

    #[derive(Default)]
    struct ControlledLauncher {
        completion_sender: StdMutex<Option<oneshot::Sender<AgentLoopOutcome>>>,
        cancellation: Arc<TestCancellation>,
    }

    impl ControlledLauncher {
        fn complete(&self, outcome: AgentLoopOutcome) {
            let Some(sender) = lock(&self.completion_sender).take() else {
                panic!("agent loop was not started");
            };
            let _ignored = sender.send(outcome);
        }

        fn cancellation_reason(&self) -> Option<String> {
            lock(&self.cancellation.reason).clone()
        }
    }

    impl AgentLoopLauncher for ControlledLauncher {
        fn start_agent_loop(
            &self,
            _request: StartAgentLoopRequest,
            _agent_run_api: Arc<dyn AgentRunApi>,
        ) -> StartedAgentLoop {
            let (sender, receiver) = oneshot::channel();
            *lock(&self.completion_sender) = Some(sender);
            StartedAgentLoop {
                completion: AgentLoopCompletion::new(async move {
                    receiver.await.unwrap_or_else(|_| AgentLoopOutcome {
                        kind: AgentLoopOutcomeKind::LoopFailed {
                            error_summary: "test completion sender dropped".to_owned(),
                        },
                        final_conversation_messages: Vec::new(),
                        total_token_count: None,
                    })
                }),
                cancellation: self.cancellation.clone(),
            }
        }
    }

    #[derive(Debug, Default)]
    struct TestCancellation {
        reason: StdMutex<Option<String>>,
    }

    impl AgentLoopCancellation for TestCancellation {
        fn cancel(&self, reason: &str) {
            let mut stored = lock(&self.reason);
            if stored.is_none() {
                *stored = Some(reason.to_owned());
            }
        }
    }

    #[derive(Default)]
    struct FakeAgentRunStore {
        runs: StdMutex<HashMap<AgentRunId, AgentRun>>,
        finish_count: AtomicUsize,
    }

    impl FakeAgentRunStore {
        fn finish_count(&self) -> usize {
            self.finish_count.load(Ordering::SeqCst)
        }
    }

    impl eos_types::Sealed for FakeAgentRunStore {}

    #[async_trait]
    impl AgentRunStore for FakeAgentRunStore {
        async fn create_run(
            &self,
            agent_run_id: &AgentRunId,
            task_id: Option<&TaskId>,
            agent_name: &str,
        ) -> Result<AgentRun, CoreError> {
            let run = AgentRun {
                id: agent_run_id.clone(),
                task_id: task_id.cloned(),
                agent_name: agent_name.to_owned(),
                terminal_payload: None,
                token_count: 0,
                error: None,
                created_at: UtcDateTime::now(),
                finished_at: None,
            };
            lock(&self.runs).insert(agent_run_id.clone(), run.clone());
            Ok(run)
        }

        async fn finish_run(
            &self,
            agent_run_id: &AgentRunId,
            terminal_payload: Option<&JsonObject>,
            token_count: i64,
            error: Option<&str>,
        ) -> Result<Option<AgentRun>, CoreError> {
            self.finish_count.fetch_add(1, Ordering::SeqCst);
            let mut runs = lock(&self.runs);
            let Some(run) = runs.get_mut(agent_run_id) else {
                return Ok(None);
            };
            run.terminal_payload = terminal_payload.cloned();
            run.token_count = token_count;
            run.error = error.map(str::to_owned);
            run.finished_at = Some(UtcDateTime::now());
            Ok(Some(run.clone()))
        }

        async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
            Ok(lock(&self.runs).get(agent_run_id).cloned())
        }

        async fn get_for_task(&self, task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
            Ok(lock(&self.runs)
                .values()
                .find(|run| run.task_id.as_ref() == Some(task_id))
                .cloned())
        }
    }

    #[derive(Default)]
    struct FakeTaskAgentRunStore {
        indexes: StdMutex<HashMap<AgentRunId, AgentRunRecordIndex>>,
        finish_count: AtomicUsize,
    }

    impl FakeTaskAgentRunStore {
        fn finish_count(&self) -> usize {
            self.finish_count.load(Ordering::SeqCst)
        }
    }

    impl eos_types::Sealed for FakeTaskAgentRunStore {}

    #[async_trait]
    impl TaskAgentRunStore for FakeTaskAgentRunStore {
        async fn create_root_task_agent_run(
            &self,
            request_id: &RequestId,
            agent_run_id: &AgentRunId,
            _agent_name: &AgentName,
        ) -> Result<CreatedTaskAgentRun, CoreError> {
            let index = AgentRunRecordIndex {
                request_id: request_id.clone(),
                agent_run_id: agent_run_id.clone(),
                task_id: root_task_id(request_id),
                kind: TaskAgentRunKind::Root,
                parent_record_dir: None,
            };
            lock(&self.indexes).insert(agent_run_id.clone(), index.clone());
            Ok(created_from_index(&index))
        }

        async fn create_workflow_task_agent_run(
            &self,
            _request_id: &RequestId,
            _agent_run_id: &AgentRunId,
            _workflow: &WorkflowCoordinates,
            _workflow_node_id: &WorkflowNodeId,
            _agent_name: &AgentName,
        ) -> Result<CreatedTaskAgentRun, CoreError> {
            Err(CoreError::Store("workflow fake not implemented".to_owned()))
        }

        async fn create_parented_task_agent_run(
            &self,
            _agent_run_id: &AgentRunId,
            _parent: &ParentAgentRunAnchor,
            _kind: ParentedAgentRunKind,
            _tool_use_id: Option<&ToolUseId>,
            _agent_name: &AgentName,
        ) -> Result<CreatedTaskAgentRun, CoreError> {
            Err(CoreError::Store("parented fake not implemented".to_owned()))
        }

        async fn finish_task_run(
            &self,
            agent_run_id: &AgentRunId,
            status: TaskStatus,
            terminal_payload: Option<&JsonObject>,
            token_count: i64,
            error: Option<&str>,
        ) -> Result<Option<TaskRun>, CoreError> {
            self.finish_count.fetch_add(1, Ordering::SeqCst);
            let Some(index) = lock(&self.indexes).get(agent_run_id).cloned() else {
                return Ok(None);
            };
            Ok(Some(TaskRun {
                task_id: index.task_id,
                agent_run_id: index.agent_run_id,
                request_id: index.request_id,
                role: TaskRole::Root,
                status,
                workflow_id: None,
                iteration_id: None,
                attempt_id: None,
                agent_name: AgentName::new("root").expect("valid agent name"),
                terminal_payload: terminal_payload.cloned(),
                token_count,
                error: error.map(str::to_owned),
                created_at: UtcDateTime::now(),
                updated_at: UtcDateTime::now(),
                finished_at: Some(UtcDateTime::now()),
            }))
        }

        async fn finish_parented_run(
            &self,
            _agent_run_id: &AgentRunId,
            _status: TaskStatus,
            _terminal_payload: Option<&JsonObject>,
            _token_count: i64,
            _error: Option<&str>,
        ) -> Result<Option<ParentedRun>, CoreError> {
            Err(CoreError::Store("parented fake not implemented".to_owned()))
        }

        async fn record_index_for_agent_run(
            &self,
            agent_run_id: &AgentRunId,
        ) -> Result<Option<AgentRunRecordIndex>, CoreError> {
            Ok(lock(&self.indexes).get(agent_run_id).cloned())
        }

        async fn get_task_run(&self, _task_id: &TaskId) -> Result<Option<TaskRun>, CoreError> {
            Err(CoreError::Store("get task fake not implemented".to_owned()))
        }

        async fn list_task_runs_for_request(
            &self,
            request_id: &RequestId,
        ) -> Result<Vec<TaskRun>, CoreError> {
            Ok(lock(&self.indexes)
                .values()
                .filter(|index| &index.request_id == request_id)
                .map(|index| task_run_from_index(index, TaskStatus::Running, None, 0, None))
                .collect())
        }

        async fn list_running_agent_runs_for_request(
            &self,
            request_id: &RequestId,
        ) -> Result<Vec<RunningRequestAgentRun>, CoreError> {
            Ok(lock(&self.indexes)
                .values()
                .filter(|index| &index.request_id == request_id)
                .map(|index| RunningRequestAgentRun {
                    request_id: index.request_id.clone(),
                    task_id: index.task_id.clone(),
                    agent_run_id: index.agent_run_id.clone(),
                    status: TaskStatus::Running,
                })
                .collect())
        }

        async fn list_parented_runs_for_parent_task(
            &self,
            _parent_task_id: &TaskId,
            _kind: ParentedAgentRunKind,
        ) -> Result<Vec<ParentedRun>, CoreError> {
            Err(CoreError::Store(
                "list parented fake not implemented".to_owned(),
            ))
        }

        async fn task_execution_index(
            &self,
            _task_id: &TaskId,
        ) -> Result<Option<TaskExecutionIndex>, CoreError> {
            Err(CoreError::Store(
                "task execution index fake not implemented".to_owned(),
            ))
        }
    }

    fn created_from_index(index: &AgentRunRecordIndex) -> CreatedTaskAgentRun {
        CreatedTaskAgentRun {
            agent_run_id: index.agent_run_id.clone(),
            task_id: index.task_id.clone(),
            record_target: AgentRunRecordTarget {
                request_id: index.request_id.clone(),
                agent_run_id: index.agent_run_id.clone(),
                task_id: index.task_id.clone(),
                task_agent_run_kind: index.kind.clone(),
                record_dir: format_record_dir(index),
            },
        }
    }

    fn task_run_from_index(
        index: &AgentRunRecordIndex,
        status: TaskStatus,
        terminal_payload: Option<&JsonObject>,
        token_count: i64,
        error: Option<&str>,
    ) -> TaskRun {
        TaskRun {
            task_id: index.task_id.clone(),
            agent_run_id: index.agent_run_id.clone(),
            request_id: index.request_id.clone(),
            role: TaskRole::Root,
            status,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            agent_name: AgentName::new("root").expect("valid agent name"),
            terminal_payload: terminal_payload.cloned(),
            token_count,
            error: error.map(str::to_owned),
            created_at: UtcDateTime::now(),
            updated_at: UtcDateTime::now(),
            finished_at: status.is_terminal_generator().then_some(UtcDateTime::now()),
        }
    }

    fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

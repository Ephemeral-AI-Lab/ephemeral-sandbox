//! `run_agent` runtime-boundary tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_agent_def::{AgentDefinition, AgentRegistry, AgentRole};
use eos_agent_message_records::{AgentMessageRecords, AgentRunRecordKind};
use eos_audit::NoopAuditSink;
use eos_engine::{
    run_agent, AgentRunControlFactory, AgentRunInput, AgentRunRegistry, AgentRunResult,
    BackgroundSessionFactory, EngineRunHandles, EventCallback, EventSourceFactory,
    ForegroundExecutorFactory, StreamEvent, ToolRegistryExtender,
};
use eos_llm_client::{LlmClient, LlmRequest, LlmStream, ProviderError, ToolSpec};
use eos_skills::SkillRegistry;
use eos_state::{AgentRun, AgentRunStore, CoreError, Sealed, TaskId, UtcDateTime};
use eos_testkit::{
    agent_def, factory_by_agent, factory_from, metadata, test_tools_root, tool_use_turn,
    FakeTransport,
};
use eos_tools::{
    BackgroundSupervisorPort, CancelPort, CancelledSubagent, ExecutionMetadata, NotificationSink,
    OutputShape, RegisteredTool, RunningBackgroundTasks, SandboxToolService, SkillToolService,
    SpawnedSubagent, StartedSubagent, StartedWorkflowHandle, SubagentLaunch, SubagentProgress,
    SystemNotification, ToolConfigSet, ToolError, ToolExecutor, ToolIntent, ToolName, ToolRegistry,
    ToolResult, WorkflowControlPort,
};
use eos_types::{AgentRunId, JsonObject, SubagentSessionId, WorkflowSessionId};
use serde_json::json;

#[derive(Debug)]
struct NoopLlmClient;

#[async_trait]
impl LlmClient for NoopLlmClient {
    async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[derive(Debug, Clone)]
struct CreateRecord {
    agent_run_id: AgentRunId,
    task_id: TaskId,
    agent_name: String,
}

#[derive(Debug, Clone)]
struct FinishRecord {
    agent_run_id: AgentRunId,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct RecordingAgentRunStore {
    creates: Mutex<Vec<CreateRecord>>,
    finishes: Mutex<Vec<FinishRecord>>,
    runs: Mutex<HashMap<AgentRunId, AgentRun>>,
}

impl RecordingAgentRunStore {
    fn creates(&self) -> Vec<CreateRecord> {
        self.creates.lock().unwrap().clone()
    }

    fn finishes(&self) -> Vec<FinishRecord> {
        self.finishes.lock().unwrap().clone()
    }
}

impl Sealed for RecordingAgentRunStore {}

#[async_trait]
impl AgentRunStore for RecordingAgentRunStore {
    async fn create_run(
        &self,
        agent_run_id: &AgentRunId,
        task_id: &TaskId,
        agent_name: &str,
        initial_messages: Option<&[JsonObject]>,
    ) -> Result<AgentRun, CoreError> {
        let run = AgentRun {
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
        };
        self.creates.lock().unwrap().push(CreateRecord {
            agent_run_id: agent_run_id.clone(),
            task_id: task_id.clone(),
            agent_name: agent_name.to_owned(),
        });
        self.runs
            .lock()
            .unwrap()
            .insert(agent_run_id.clone(), run.clone());
        Ok(run)
    }

    async fn finish_run(
        &self,
        agent_run_id: &AgentRunId,
        message_history: Option<&[JsonObject]>,
        terminal_tool_result: Option<&JsonObject>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<AgentRun>, CoreError> {
        self.finishes.lock().unwrap().push(FinishRecord {
            agent_run_id: agent_run_id.clone(),
            error: error.map(str::to_owned),
        });
        let mut runs = self.runs.lock().unwrap();
        let Some(run) = runs.get_mut(agent_run_id) else {
            return Ok(None);
        };
        run.message_history = message_history.map(<[_]>::to_vec);
        run.terminal_tool_result = terminal_tool_result.cloned();
        run.token_count = token_count;
        run.error = error.map(str::to_owned);
        run.finished_at = Some(UtcDateTime::now());
        Ok(Some(run.clone()))
    }

    async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
        Ok(self.runs.lock().unwrap().get(agent_run_id).cloned())
    }

    async fn get_for_task(&self, task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
        Ok(self
            .runs
            .lock()
            .unwrap()
            .values()
            .find(|run| run.task_id == *task_id)
            .cloned())
    }
}

struct CannedExecutor(ToolResult);

#[async_trait]
impl ToolExecutor for CannedExecutor {
    async fn execute(
        &self,
        _input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        Ok(self.0.clone())
    }
}

fn submit_root_tool(result: ToolResult) -> RegisteredTool {
    RegisteredTool::new(
        ToolName::SubmitRootOutcome,
        ToolIntent::ReadOnly,
        true,
        ToolSpec::new(
            "submit_root_outcome",
            "test terminal",
            json!({"type": "object"})
                .as_object()
                .expect("schema object")
                .clone(),
            None,
        ),
        OutputShape::Text,
        Arc::new(CannedExecutor(result)),
    )
}

fn terminal_extender(result: ToolResult) -> ToolRegistryExtender {
    Arc::new(move |registry: &mut ToolRegistry| {
        registry.register(submit_root_tool(result.clone()));
    })
}

#[derive(Debug, Clone)]
struct CancelRecord {
    reason: String,
}

#[derive(Debug, Default)]
struct RecordingBackgroundSupervisor {
    cancels: Mutex<Vec<CancelRecord>>,
}

impl RecordingBackgroundSupervisor {
    fn cancels(&self) -> Vec<CancelRecord> {
        self.cancels.lock().unwrap().clone()
    }
}

impl eos_tools::ports::Sealed for RecordingBackgroundSupervisor {}

fn empty_report() -> RunningBackgroundTasks {
    RunningBackgroundTasks {
        total: 0,
        subagents: 0,
        workflows: 0,
        command_sessions: 0,
    }
}

#[async_trait]
impl BackgroundSupervisorPort for RecordingBackgroundSupervisor {
    async fn spawn(
        &self,
        _ctx: &ExecutionMetadata,
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
        self.cancels.lock().unwrap().push(CancelRecord {
            reason: reason.to_owned(),
        });
        empty_report()
    }
}

fn root_agent() -> AgentDefinition {
    agent_def("root", AgentRole::Root, &[], &["submit_root_outcome"])
}

fn advisor_root_agent() -> AgentDefinition {
    agent_def(
        "root",
        AgentRole::Root,
        &["ask_advisor"],
        &["submit_root_outcome"],
    )
}

fn advisor_agent() -> AgentDefinition {
    agent_def(
        "advisor",
        AgentRole::Helper,
        &[],
        &["submit_advisor_feedback"],
    )
}

fn unknown_tool_agent() -> AgentDefinition {
    agent_def(
        "root",
        AgentRole::Root,
        &["not_a_tool"],
        &["submit_root_outcome"],
    )
}

struct Harness {
    store: Arc<RecordingAgentRunStore>,
    records: Option<AgentMessageRecords>,
    handles: EngineRunHandles,
}

fn handles(
    agents: Vec<AgentDefinition>,
    source_factory: EventSourceFactory,
    extender: Option<ToolRegistryExtender>,
    records: Option<AgentMessageRecords>,
) -> Harness {
    let store = Arc::new(RecordingAgentRunStore::default());
    let registry: AgentRegistry = agents.into_iter().collect();
    let handles = EngineRunHandles {
        agent_run_store: store.clone(),
        llm_client: Arc::new(NoopLlmClient),
        event_source_factory: Some(source_factory),
        agent_registry: Arc::new(registry),
        tool_config: Arc::new(
            ToolConfigSet::load_from_dir(&test_tools_root()).expect("tool config loads"),
        ),
        sandbox_service: SandboxToolService::new(Arc::new(FakeTransport)),
        root_submission: None,
        skill_service: SkillToolService::new(Arc::new(SkillRegistry::new())),
        tool_registry_extender: extender,
        audit: Arc::new(NoopAuditSink),
        message_records: records.clone(),
        workspace_root: "/tmp".to_owned(),
    };
    Harness {
        store,
        records,
        handles,
    }
}

fn input(
    agent: AgentDefinition,
    agent_run_id: AgentRunId,
    task_id: TaskId,
    request_id: &str,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) -> AgentRunInput {
    let mut tool_metadata = metadata();
    tool_metadata.agent_name = agent.name.as_str().to_owned();
    tool_metadata.agent_run_id = Some(agent_run_id.clone());
    tool_metadata.request_id = Some(request_id.parse().expect("request id"));
    tool_metadata.task_id = Some(task_id.clone());
    tool_metadata.workspace_root = "/tmp".to_owned();

    let foreground = Arc::new(eos_engine::ForegroundExecutorFactory.create(agent_run_id.clone()));
    AgentRunInput {
        agent,
        initial_messages: vec![eos_llm_client::Message::from_user_text("start")],
        task_id: Some(task_id),
        agent_run_id,
        tool_metadata,
        attempt_submission: None,
        workflow_control: None,
        background_supervisor,
        command_session_supervisor: None,
        notifier: eos_engine::NotificationService::new(),
        cancellation: eos_engine::AgentRunCancellation::new(),
        foreground,
        agent_run_registry: None,
        persist_agent_run: true,
        record_kind: AgentRunRecordKind::Root,
    }
}

async fn run_root_success(
    harness: &Harness,
    agent_run_id: AgentRunId,
    task_id: TaskId,
) -> AgentRunResult {
    run_agent(
        &harness.handles,
        input(root_agent(), agent_run_id, task_id, "req_runtime", None),
        None,
    )
    .await
}

async fn node_finish_status(records: &AgentMessageRecords, agent_run_id: &AgentRunId) -> String {
    let events = records
        .read_events(agent_run_id, 0)
        .await
        .expect("read message-record events");
    events
        .iter()
        .find(|event| event.kind == "node_finished")
        .and_then(|event| event.payload.get("status"))
        .and_then(serde_json::Value::as_str)
        .expect("node_finished status")
        .to_owned()
}

#[tokio::test]
async fn run_agent_finishes_agent_run_on_setup_error() {
    let agent = unknown_tool_agent();
    let agent_run_id: AgentRunId = "run_setup_error".parse().expect("run id");
    let task_id: TaskId = "task_setup_error".parse().expect("task id");
    let harness = handles(vec![agent.clone()], factory_from(Vec::new()), None, None);

    let result = run_agent(
        &harness.handles,
        input(
            agent,
            agent_run_id.clone(),
            task_id.clone(),
            "req_setup_error",
            None,
        ),
        None,
    )
    .await;

    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("unknown tool: not_a_tool")),
        "{result:?}"
    );
    let creates = harness.store.creates();
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0].agent_run_id, agent_run_id);
    assert_eq!(creates[0].task_id, task_id);
    assert_eq!(creates[0].agent_name, "root");
    let finishes = harness.store.finishes();
    assert_eq!(finishes.len(), 1);
    assert_eq!(finishes[0].agent_run_id, agent_run_id);
    assert!(finishes[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("unknown tool: not_a_tool")));
}

#[tokio::test]
async fn run_agent_finishes_message_record_completed_on_success() {
    let records_root = tempfile::tempdir().expect("records dir");
    let records = AgentMessageRecords::new(records_root.path());
    let harness = handles(
        vec![root_agent()],
        factory_from(vec![tool_use_turn(
            "toolu_stop",
            "submit_root_outcome",
            json!({"summary": "done"}),
        )]),
        Some(terminal_extender(ToolResult::ok("done"))),
        Some(records.clone()),
    );
    let agent_run_id: AgentRunId = "run_record_completed".parse().expect("run id");
    let task_id: TaskId = "task_record_completed".parse().expect("task id");

    let result = run_root_success(&harness, agent_run_id.clone(), task_id).await;

    assert!(result.error.is_none(), "{result:?}");
    assert!(result
        .terminal_result
        .as_ref()
        .is_some_and(|result| result.is_terminal));
    assert_eq!(
        node_finish_status(harness.records.as_ref().expect("records"), &agent_run_id).await,
        "completed"
    );
}

#[tokio::test]
async fn run_agent_finishes_message_record_failed_on_stream_error() {
    let records_root = tempfile::tempdir().expect("records dir");
    let records = AgentMessageRecords::new(records_root.path());
    let harness = handles(
        vec![root_agent()],
        factory_from(vec![Vec::new()]),
        Some(terminal_extender(ToolResult::ok("done"))),
        Some(records.clone()),
    );
    let agent_run_id: AgentRunId = "run_record_failed".parse().expect("run id");
    let task_id: TaskId = "task_record_failed".parse().expect("task id");

    let result = run_agent(
        &harness.handles,
        input(
            root_agent(),
            agent_run_id.clone(),
            task_id,
            "req_record_failed",
            None,
        ),
        None,
    )
    .await;

    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("provider stream ended without assistant completion")));
    assert_eq!(
        node_finish_status(harness.records.as_ref().expect("records"), &agent_run_id).await,
        "failed"
    );
}

#[tokio::test]
async fn run_agent_finalizes_background_handles_after_query_error() {
    let background = Arc::new(RecordingBackgroundSupervisor::default());
    let harness = handles(
        vec![root_agent()],
        factory_from(vec![Vec::new()]),
        Some(terminal_extender(ToolResult::ok("done"))),
        None,
    );
    let agent_run_id: AgentRunId = "run_background_finalize".parse().expect("run id");
    let task_id: TaskId = "task_background_finalize".parse().expect("task id");

    let result = run_agent(
        &harness.handles,
        input(
            root_agent(),
            agent_run_id.clone(),
            task_id,
            "req_background_finalize",
            Some(background.clone()),
        ),
        None,
    )
    .await;

    assert!(result.error.is_some());
    let cancels = background.cancels();
    assert_eq!(cancels.len(), 1);
    assert!(cancels[0].reason.contains("engine run failed"));
    assert!(cancels[0]
        .reason
        .contains("provider stream ended without assistant completion"));
}

#[tokio::test]
async fn run_agent_finalizes_background_handles_after_tool_stop() {
    let background = Arc::new(RecordingBackgroundSupervisor::default());
    let harness = handles(
        vec![root_agent()],
        factory_from(vec![tool_use_turn(
            "toolu_stop",
            "submit_root_outcome",
            json!({"summary": "done"}),
        )]),
        Some(terminal_extender(ToolResult::ok("done"))),
        None,
    );
    let agent_run_id: AgentRunId = "run_background_tool_stop".parse().expect("run id");
    let task_id: TaskId = "task_background_tool_stop".parse().expect("task id");

    let result = run_agent(
        &harness.handles,
        input(
            root_agent(),
            agent_run_id.clone(),
            task_id,
            "req_background_tool_stop",
            Some(background.clone()),
        ),
        None,
    )
    .await;

    assert!(result.error.is_none(), "{result:?}");
    let cancels = background.cancels();
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0].reason, "parent agent submitted its terminal");
}

#[tokio::test]
async fn run_agent_finalizes_background_handles_after_terminal_not_submitted() {
    let background = Arc::new(RecordingBackgroundSupervisor::default());
    let turns = std::iter::repeat_with(|| eos_testkit::text_turn("still thinking"))
        .take(12)
        .collect();
    let harness = handles(
        vec![root_agent()],
        factory_from(turns),
        Some(terminal_extender(ToolResult::ok("done"))),
        None,
    );
    let agent_run_id: AgentRunId = "run_background_no_terminal".parse().expect("run id");
    let task_id: TaskId = "task_background_no_terminal".parse().expect("task id");

    let result = run_agent(
        &harness.handles,
        input(
            root_agent(),
            agent_run_id.clone(),
            task_id,
            "req_background_no_terminal",
            Some(background.clone()),
        ),
        None,
    )
    .await;

    assert!(result.error.is_none(), "{result:?}");
    assert!(result.terminal_result.is_none());
    let cancels = background.cancels();
    assert_eq!(cancels.len(), 1);
    assert_eq!(
        cancels[0].reason,
        "parent agent exited without submitting a terminal tool"
    );
}

#[tokio::test]
async fn run_agent_routes_ask_advisor_through_child_advisor_run() {
    let advisor_summary =
        "Tool selection correct. Quality of synthesis is supported. Residual risks: None.";
    let harness = handles(
        vec![advisor_root_agent(), advisor_agent()],
        factory_by_agent(vec![
            (
                "root",
                vec![
                    tool_use_turn(
                        "toolu_advisor",
                        "ask_advisor",
                        json!({
                            "tool_name": "submit_root_outcome",
                            "tool_payload": {"summary": "done"}
                        }),
                    ),
                    tool_use_turn(
                        "toolu_stop",
                        "submit_root_outcome",
                        json!({"summary": "done"}),
                    ),
                ],
            ),
            (
                "advisor",
                vec![tool_use_turn(
                    "toolu_feedback",
                    "submit_advisor_feedback",
                    json!({
                        "verdict": "approve",
                        "summary": advisor_summary
                    }),
                )],
            ),
        ]),
        Some(terminal_extender(ToolResult::ok("done"))),
        None,
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_events = seen.clone();
    let callback: EventCallback = Arc::new(move |event| {
        seen_events.lock().expect("event lock").push(event.clone());
    });
    let agent_run_id: AgentRunId = "run_advisor".parse().expect("run id");
    let task_id: TaskId = "task_advisor".parse().expect("task id");

    let result = run_agent(
        &harness.handles,
        input(
            advisor_root_agent(),
            agent_run_id,
            task_id,
            "req_advisor",
            None,
        ),
        Some(&callback),
    )
    .await;

    assert!(result.error.is_none(), "{result:?}");
    assert!(result
        .terminal_result
        .as_ref()
        .is_some_and(|result| result.is_terminal));
    let events = seen.lock().expect("event lock");
    assert!(events.iter().any(|event| {
        matches!(
            event,
            StreamEvent::ToolExecutionCompleted {
                tool_name,
                output,
                is_error: false,
                is_terminal: false,
                metadata,
                ..
            } if tool_name == "ask_advisor"
                && output == advisor_summary
                && metadata["helper_role"] == json!("advisor")
                && metadata["verdict"] == json!("approve")
        )
    }));
}

// ---------------------------------------------------------------------------
// Per-agent-run ownership (spec §6, §17): each run owns its own
// AgentRunControl / NotificationService; the registry arbitrates finalization.
// ---------------------------------------------------------------------------

fn engine_handles(store: Arc<RecordingAgentRunStore>) -> EngineRunHandles {
    EngineRunHandles {
        agent_run_store: store,
        // NoopLlmClient yields an empty stream → the loop reaches finalization with
        // a framework error; the test asserts the finalization arbitration, not the
        // run outcome.
        llm_client: Arc::new(NoopLlmClient),
        event_source_factory: None,
        agent_registry: Arc::new(Vec::new().into_iter().collect::<AgentRegistry>()),
        tool_config: Arc::new(
            ToolConfigSet::load_from_dir(&test_tools_root()).expect("tool config loads"),
        ),
        sandbox_service: SandboxToolService::new(Arc::new(FakeTransport)),
        root_submission: None,
        skill_service: SkillToolService::new(Arc::new(SkillRegistry::new())),
        tool_registry_extender: None,
        audit: Arc::new(NoopAuditSink),
        message_records: None,
        workspace_root: "/tmp".to_owned(),
    }
}

fn control_factory_for(handles: EngineRunHandles) -> AgentRunControlFactory {
    AgentRunControlFactory::new(
        ForegroundExecutorFactory,
        // A long interval keeps the per-run heartbeat idle for the duration of
        // the test (no command sessions → no RPC); the control's drop aborts it.
        BackgroundSessionFactory::new(
            handles,
            Arc::new(FakeTransport),
            std::time::Duration::from_secs(3600),
            Arc::new(std::sync::OnceLock::new()),
        ),
    )
}

fn control_factory() -> AgentRunControlFactory {
    control_factory_for(engine_handles(Arc::new(RecordingAgentRunStore::default())))
}

/// A `TaskStore` that does nothing — `cancel_agent_run` never touches it, and the
/// cancel test exercises only the agent-run arbitration.
#[derive(Debug, Default)]
struct NoopTaskStore;

impl Sealed for NoopTaskStore {}

#[async_trait]
impl eos_state::TaskStore for NoopTaskStore {
    async fn insert_task(&self, _task: &eos_state::Task) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get(&self, _id: &TaskId) -> Result<Option<eos_state::Task>, CoreError> {
        Ok(None)
    }
    async fn set_task_status_if_current(
        &self,
        _id: &TaskId,
        _expected: eos_state::TaskStatus,
        _status: eos_state::TaskStatus,
        _outcomes: Option<&[eos_state::ExecutionTaskOutcome]>,
        _terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<eos_state::Task>, CoreError> {
        Ok(None)
    }
    async fn latch_attempt_tasks_cancelled(
        &self,
        _attempt_id: &eos_state::AttemptId,
        _ids: &[TaskId],
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_for_request(
        &self,
        _request_id: &eos_types::RequestId,
    ) -> Result<Vec<eos_state::Task>, CoreError> {
        Ok(Vec::new())
    }
}

/// Spec §6.4 + §12.3 (finalization arbitration, the advisor's gate test):
/// `run_agent` and a concurrent `cancel_agent_run` race to finalize one
/// registered run. Under *any* interleaving the `Running -> Claimed` CAS lets
/// exactly one side finalize the durable row, and a second cancel is a clean
/// no-op.
#[tokio::test]
async fn concurrent_cancel_and_run_finalize_exactly_once() {
    let store = Arc::new(RecordingAgentRunStore::default());
    let handles = engine_handles(store.clone());
    // The control's finalization store must be the SAME store `run_agent` writes,
    // so "exactly one finish_run" is directly assertable.
    let factory = control_factory_for(handles.clone());
    let registry = AgentRunRegistry::new();
    let run_id: AgentRunId = "run-cancel".parse().expect("run id");
    let task_id: TaskId = "task-cancel".parse().expect("task id");

    let control = factory.persisted(run_id.clone(), task_id.clone());
    registry.insert(control.clone());

    let cancel_port = eos_engine::EngineCancelPort::new(
        registry.clone(),
        Arc::new(NoopTaskStore),
        Arc::new(std::sync::OnceLock::new()),
    );

    let input = AgentRunInput {
        agent: agent_def("root", AgentRole::Root, &[], &["submit_root_outcome"]),
        initial_messages: vec![eos_llm_client::Message::from_user_text("start")],
        task_id: Some(task_id.clone()),
        agent_run_id: run_id.clone(),
        tool_metadata: metadata(),
        attempt_submission: None,
        workflow_control: None,
        background_supervisor: None,
        command_session_supervisor: None,
        notifier: control.notifications(),
        cancellation: control.cancellation(),
        foreground: control.foreground(),
        agent_run_registry: Some(registry.clone()),
        persist_agent_run: true,
        record_kind: AgentRunRecordKind::Root,
    };

    // Race run_agent against cancel. The CAS guarantees exactly one finalizer.
    let run_fut = run_agent(&handles, input, None);
    let cancel_fut = cancel_port.cancel_agent_run(&run_id, "external cancel");
    let (_run, cancel_res) = tokio::join!(run_fut, cancel_fut);
    cancel_res.expect("cancel_agent_run ok");

    assert_eq!(
        store.finishes().len(),
        1,
        "exactly one finalizer (run_agent OR cancel) finishes the durable row"
    );
    assert!(
        registry.get(&run_id).is_none(),
        "the live-run entry is removed after finalization"
    );

    // A second cancel sees no live entry and is a clean no-op.
    cancel_port
        .cancel_agent_run(&run_id, "again")
        .await
        .expect("second cancel is a no-op");
    assert_eq!(
        store.finishes().len(),
        1,
        "second cancel does not re-finalize"
    );
    drop(control);
}

/// Spec §17 (Runtime Wiring + Background Notifications): two live runs own
/// independent notification queues, and `AgentRunControl::notifications()`
/// returns clones of the *same* queue (the instance-identity invariant). A
/// notification enqueued for run A must never be drainable by run B.
#[tokio::test]
async fn per_run_controls_own_independent_notifiers() {
    let factory = control_factory();
    let run_a: AgentRunId = "run-a".parse().expect("run a");
    let run_b: AgentRunId = "run-b".parse().expect("run b");
    let a = factory.persisted(run_a, "task-a".parse().expect("task a"));
    let b = factory.persisted(run_b, "task-b".parse().expect("task b"));

    a.notifications()
        .notify_system(SystemNotification {
            event: "completed".to_owned(),
            message: "from-a".to_owned(),
        })
        .await
        .expect("enqueue into A");

    // Cross-agent isolation: B's notifier never sees A's notification.
    assert!(
        b.notifications().drain().await.is_empty(),
        "workflow agent B must not drain workflow agent A's completion"
    );
    // Instance identity: a *different clone* of A's queue drains the same item.
    let drained = a.notifications().drain().await;
    assert_eq!(drained.len(), 1, "A's own notifier delivers via any clone");
    assert_eq!(drained[0].message, "from-a");
}

/// Spec §6.4 (+ the natural-vs-cancel finalization arbitration): the registry
/// resolves a live run by task, the `Running -> Claimed` claim is a one-shot CAS
/// (repeat claims no-op), and a claimed entry is no longer addressable as live.
#[tokio::test]
async fn registry_claim_is_one_shot_and_resolves_by_task() {
    let factory = control_factory();
    let run_a: AgentRunId = "run-a".parse().expect("run a");
    let task_a: TaskId = "task-a".parse().expect("task a");
    let control = factory.persisted(run_a.clone(), task_a.clone());

    let registry = AgentRunRegistry::new();
    registry.insert(control);
    assert_eq!(registry.agent_run_for_task(&task_a), Some(run_a.clone()));
    assert!(registry.get(&run_a).is_some(), "live run is addressable");

    assert!(
        registry.begin_cancel(&run_a).is_some(),
        "first claim wins and returns the control"
    );
    assert!(
        registry.begin_cancel(&run_a).is_none(),
        "second claim sees Claimed and no-ops (natural-vs-cancel arbitration)"
    );
    assert!(
        registry.get(&run_a).is_none(),
        "a claimed entry is no longer 'Running'"
    );

    registry.finish_cancel(&run_a);
    assert!(
        registry.agent_run_for_task(&task_a).is_none(),
        "finish_cancel removes both indices"
    );
}

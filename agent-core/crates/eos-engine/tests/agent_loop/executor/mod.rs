use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, MutexGuard};

use async_trait::async_trait;
use eos_llm_client::{StopReason, ToolSpec};
use eos_tool::{
    ExecutionMetadata, OutputShape, RegisteredTool, ToolError, ToolExecutor, ToolIntent,
    ToolRegistry,
};
use eos_types::{
    AgentLoopCancellationHandle, AgentLoopLauncher, AgentRunError, AgentRunOutcome,
    AgentRunRecordDir, AgentRunRecordTarget, AgentRunStatus, RequestId, SpawnAgentRequest,
    TaskAgentRunKind, TaskId,
};
use tokio::sync::Notify;
use tokio::time::{timeout, Duration};

use super::*;
use crate::provider_stream::EngineStream;
use crate::AgentLoopToolRegistryBuildInput;
use crate::TokioAgentLoopLauncher;

#[tokio::test]
async fn foreground_tool_batch_uses_bounded_fan_out_and_ordered_fan_in() {
    let gate = Arc::new(TwoToolGate::default());
    let registry_factory = FixedRegistryFactory::new(registry_with_coordinated_tools(&gate));
    let state = AgentLoopState::from_request(
        test_start_request(),
        &registry_factory,
        AgentLoopRunServices::inert(),
        Arc::new(UnusedAgentRunApi),
    )
    .expect("state builds");
    let executor = test_executor();
    let calls = vec![
        ToolUseRequest {
            tool_use_id: "toolu_read".parse().expect("valid tool use id"),
            name: ToolName::ReadFile.as_str().to_owned(),
            input: JsonObject::new(),
        },
        ToolUseRequest {
            tool_use_id: "toolu_edit".parse().expect("valid tool use id"),
            name: ToolName::EditFile.as_str().to_owned(),
            input: JsonObject::new(),
        },
    ];

    let outcome = timeout(
        Duration::from_secs(1),
        executor.dispatch_tool_batch(&state, &calls),
    )
    .await
    .expect("both tools start before either completes")
    .expect("dispatch succeeds");

    assert_eq!(gate.started.load(Ordering::SeqCst), 2);
    assert_eq!(outcome.tool_results.len(), 2);
    assert_eq!(
        outcome.tool_results[0],
        result_block(&calls[0].tool_use_id, &ToolResult::ok("toolu_read"))
    );
    assert_eq!(
        outcome.tool_results[1],
        result_block(&calls[1].tool_use_id, &ToolResult::ok("toolu_edit"))
    );
}

#[tokio::test]
async fn cancellation_during_provider_stream_finishes_promptly() {
    let stream_started = Arc::new(Notify::new());
    let launcher = TokioAgentLoopLauncher::new(
        Arc::new(PendingStreamSource {
            stream_started: stream_started.clone(),
        }),
        Arc::new(FixedRegistryFactory::new(ToolRegistry::new())),
        Arc::new(TestMetadataReader),
    );
    let started = launcher.start_agent_loop(test_start_request(), Arc::new(UnusedAgentRunApi));

    timeout(Duration::from_secs(1), stream_started.notified())
        .await
        .expect("provider stream starts");
    started.cancellation.cancel("caller cancelled");

    let outcome = timeout(Duration::from_millis(200), started.completion.wait())
        .await
        .expect("cancellation completes loop promptly");
    assert!(matches!(
        outcome.kind,
        AgentLoopOutcomeKind::LoopFailed { ref error_summary }
            if error_summary.contains("caller cancelled")
    ));
}

#[tokio::test]
async fn cancellation_after_assistant_completion_skips_tool_dispatch() {
    let (cancellation, cancel_signal) = AgentLoopCancelSignal::for_test_pair();
    let executions = Arc::new(AtomicUsize::new(0));
    let executor = AgentLoopExecutor {
        provider_stream_source: AgentLoopProviderStream::Static(Arc::new(
            CancelOnCompletionSource {
                cancellation: cancellation.clone(),
            },
        )),
        tool_registry_factory: Arc::new(FixedRegistryFactory::new(registry_with_counting_tool(
            &executions,
        ))),
        execution_metadata_reader: Arc::new(TestMetadataReader),
        cancel_signal,
        background_sessions: None,
        hook_stores: None,
        run_outputs: AgentRunOutputs::new(),
        agent_run_api: Arc::new(UnusedAgentRunApi),
    };

    let outcome = timeout(
        Duration::from_secs(1),
        executor.execute_agent_loop(test_start_request()),
    )
    .await
    .expect("loop finishes");

    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(matches!(
        outcome.kind,
        AgentLoopOutcomeKind::LoopFailed { ref error_summary }
            if error_summary.contains("caller cancelled")
    ));
}

fn test_executor() -> AgentLoopExecutor {
    AgentLoopExecutor {
        provider_stream_source: AgentLoopProviderStream::Static(Arc::new(EmptyStreamSource)),
        tool_registry_factory: Arc::new(UnusedRegistryFactory),
        execution_metadata_reader: Arc::new(TestMetadataReader),
        cancel_signal: AgentLoopCancelSignal::for_test(),
        background_sessions: None,
        hook_stores: None,
        run_outputs: AgentRunOutputs::new(),
        agent_run_api: Arc::new(UnusedAgentRunApi),
    }
}

fn registry_with_coordinated_tools(gate: &Arc<TwoToolGate>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for name in [ToolName::ReadFile, ToolName::EditFile] {
        registry.register(RegisteredTool::new(
            name,
            ToolIntent::ReadOnly,
            false,
            ToolSpec::new(
                name.as_str(),
                "coordinated test tool",
                JsonObject::new(),
                None,
            ),
            OutputShape::Text,
            Arc::new(CoordinatedTool { gate: gate.clone() }),
        ));
    }
    registry
}

fn registry_with_counting_tool(executions: &Arc<AtomicUsize>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(RegisteredTool::new(
        ToolName::ReadFile,
        ToolIntent::ReadOnly,
        false,
        ToolSpec::new(
            ToolName::ReadFile.as_str(),
            "counting test tool",
            JsonObject::new(),
            None,
        ),
        OutputShape::Text,
        Arc::new(CountingTool {
            executions: executions.clone(),
        }),
    ));
    registry
}

fn test_start_request() -> StartAgentLoopRequest {
    let request_id = RequestId::new_v4();
    let agent_run_id = AgentRunId::new_v4();
    let task_id = TaskId::new_v4();
    StartAgentLoopRequest {
        record_target: AgentRunRecordTarget {
            request_id,
            agent_run_id,
            task_id,
            task_agent_run_kind: TaskAgentRunKind::Root,
            record_dir: AgentRunRecordDir::new("requests/test/root-task-test/agent-run-test"),
        },
        initial_messages: vec![AgentLoopMessage::UserMessage(Message::from_user_text(
            "run both tools",
        ))],
        model_key: "test-model".to_owned(),
        max_completion_tokens: 100,
        tool_call_limit: 8,
    }
}

struct FixedRegistryFactory {
    registry: StdMutex<Option<ToolRegistry>>,
}

impl FixedRegistryFactory {
    fn new(registry: ToolRegistry) -> Self {
        Self {
            registry: StdMutex::new(Some(registry)),
        }
    }
}

impl AgentLoopToolRegistryFactory for FixedRegistryFactory {
    fn build_tool_registry(
        &self,
        _input: AgentLoopToolRegistryBuildInput,
    ) -> Result<ToolRegistry, EngineError> {
        lock(&self.registry).take().ok_or_else(|| {
            EngineError::Internal("registry factory called more than once".to_owned())
        })
    }
}

#[derive(Default)]
struct TwoToolGate {
    started: AtomicUsize,
    notify: Notify,
}

struct CoordinatedTool {
    gate: Arc<TwoToolGate>,
}

#[async_trait]
impl ToolExecutor for CoordinatedTool {
    async fn execute(
        &self,
        _input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        self.gate.started.fetch_add(1, Ordering::SeqCst);
        self.gate.notify.notify_waiters();
        loop {
            if self.gate.started.load(Ordering::SeqCst) >= 2 {
                break;
            }
            self.gate.notify.notified().await;
        }
        Ok(ToolResult::ok(
            ctx.tool_use_id
                .as_ref()
                .expect("tool use id")
                .as_str()
                .to_owned(),
        ))
    }
}

struct CountingTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolExecutor for CountingTool {
    async fn execute(
        &self,
        _input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok("executed"))
    }
}

struct TestMetadataReader;

#[async_trait]
impl ToolExecutionMetadataReader for TestMetadataReader {
    async fn agent_run_snapshot(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunRuntimeSnapshot, EngineError> {
        Ok(AgentRunRuntimeSnapshot {
            agent_run_id: agent_run_id.clone(),
            agent_name: "root".to_owned(),
            request_id: None,
            task_id: None,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            sandbox_id: None,
            workspace_root: String::new(),
            is_isolated_workspace_mode: false,
        })
    }

    async fn build_execution_metadata(
        &self,
        input: ExecutionMetadataBuildInput,
    ) -> Result<ExecutionMetadata, EngineError> {
        Ok(ExecutionMetadata {
            agent_name: "root".to_owned(),
            agent_run_id: Some(input.agent_run_id),
            request_id: None,
            task_id: None,
            attempt_id: None,
            workflow_id: None,
            work_item_id: None,
            tool_use_id: Some(input.tool_use_id),
            sandbox_invocation_id: None,
            sandbox_id: None,
            is_isolated_workspace_mode: false,
            workspace_root: String::new(),
            conversation: input.conversation,
        })
    }
}

struct EmptyStreamSource;

#[async_trait]
impl ProviderStreamSource for EmptyStreamSource {
    async fn stream(&self, _request: &LlmRequest) -> Result<EngineStream, EngineError> {
        Ok(Box::pin(stream::empty()))
    }
}

struct PendingStreamSource {
    stream_started: Arc<Notify>,
}

#[async_trait]
impl ProviderStreamSource for PendingStreamSource {
    async fn stream(&self, _request: &LlmRequest) -> Result<EngineStream, EngineError> {
        self.stream_started.notify_one();
        Ok(Box::pin(stream::pending::<
            Result<AgentRunStreamEvent, EngineError>,
        >()))
    }
}

struct CancelOnCompletionSource {
    cancellation: AgentLoopCancellationHandle,
}

#[async_trait]
impl ProviderStreamSource for CancelOnCompletionSource {
    async fn stream(&self, _request: &LlmRequest) -> Result<EngineStream, EngineError> {
        let cancellation = self.cancellation.clone();
        Ok(Box::pin(stream::once(async move {
            cancellation.cancel("caller cancelled");
            Ok(assistant_complete_with_tool_use())
        })))
    }
}

fn assistant_complete_with_tool_use() -> AgentRunStreamEvent {
    AgentRunStreamEvent::AssistantMessageComplete {
        agent_name: String::new(),
        agent_run_id: None,
        payload: Box::new(crate::AssistantMessageComplete {
            message: Message {
                role: eos_llm_client::MessageRole::Assistant,
                content: vec![ContentBlock::ToolUse {
                    tool_use_id: "toolu_read".parse().expect("tool use id"),
                    name: ToolName::ReadFile.as_str().to_owned(),
                    input: JsonObject::new(),
                }],
            },
            usage: UsageSnapshot::default(),
            stop_reason: Some(StopReason::ToolUse),
        }),
    }
}

struct UnusedRegistryFactory;

impl AgentLoopToolRegistryFactory for UnusedRegistryFactory {
    fn build_tool_registry(
        &self,
        _input: AgentLoopToolRegistryBuildInput,
    ) -> Result<ToolRegistry, EngineError> {
        Err(EngineError::Internal(
            "registry factory not used by dispatch test".to_owned(),
        ))
    }
}

struct UnusedAgentRunApi;

#[async_trait]
impl AgentRunApi for UnusedAgentRunApi {
    async fn spawn_agent(&self, _request: SpawnAgentRequest) -> Result<AgentRunId, AgentRunError> {
        Err(AgentRunError::Internal(
            "agent API not used by dispatch test".to_owned(),
        ))
    }

    async fn wait_for_agent_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        Ok(AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: AgentRunStatus::Failed,
            submission_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some("unused".to_owned()),
        })
    }

    async fn poll_agent_run_outcome(
        &self,
        _agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
        Ok(None)
    }

    async fn cancel_agent_run(
        &self,
        _agent_run_id: &AgentRunId,
        _reason: &str,
    ) -> Result<(), AgentRunError> {
        Ok(())
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

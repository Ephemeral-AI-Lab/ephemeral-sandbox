//! Query-loop integration tests for tool dispatch, terminal stop semantics,
//! prompt-report capture, and notification insertion.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use eos_engine::{
    run_query, EngineError, EngineStream, EventSource, NotificationService,
    PromptReportRecorder, QueryContext, QueryExitReason, QueryStream, StreamEvent,
};
use eos_llm_client::{ContentBlock, LlmRequest, Message, ToolSpec};
use eos_testkit::{metadata, run_until, tool_use_turn, ScriptedSource};
use eos_tools::{
    ExecutionMetadata, NotificationSink, OutputShape, RegisteredTool, SystemNotification,
    ToolError, ToolExecutor, ToolIntent, ToolName, ToolRegistry, ToolResult,
};
use eos_types::{AgentRunId, JsonObject};
use futures::StreamExt;
use serde_json::{json, Value};

struct CannedExecutor {
    result: ToolResult,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolExecutor for CannedExecutor {
    async fn execute(
        &self,
        _input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

struct RecordingSource {
    turns: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
    requests: tokio::sync::Mutex<Vec<LlmRequest>>,
}

impl RecordingSource {
    fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            turns: tokio::sync::Mutex::new(turns),
            requests: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    async fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl EventSource for RecordingSource {
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
        self.requests.lock().await.push(request.clone());
        let mut turns = self.turns.lock().await;
        let events = if turns.is_empty() {
            Vec::new()
        } else {
            turns.remove(0)
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

fn spec(name: ToolName) -> ToolSpec {
    ToolSpec::new(
        name.as_str(),
        "test tool",
        json!({"type": "object"})
            .as_object()
            .expect("schema object")
            .clone(),
        None,
    )
}

fn canned_tool(
    name: ToolName,
    is_terminal: bool,
    result: ToolResult,
) -> (RegisteredTool, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = CannedExecutor {
        result,
        calls: calls.clone(),
    };
    (
        RegisteredTool::new(
            name,
            ToolIntent::ReadOnly,
            is_terminal,
            spec(name),
            OutputShape::Text,
            Arc::new(executor),
        ),
        calls,
    )
}

fn registry(tools: Vec<RegisteredTool>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.register(tool);
    }
    registry
}

fn ctx(
    source: Arc<dyn EventSource>,
    registry: ToolRegistry,
    terminal_tools: BTreeSet<ToolName>,
) -> QueryContext {
    QueryContext {
        tool_registry: Arc::new(registry),
        cwd: PathBuf::new(),
        model: "test-model".to_owned(),
        system_prompt: "system".to_owned(),
        max_tokens: 128,
        tool_call_limit: 8,
        agent_name: "root".to_owned(),
        agent_run_id: AgentRunId::new_v4(),
        task_id: None,
        tool_calls_used: 0,
        text_only_no_terminal_turns: 0,
        tool_metadata: metadata(),
        terminal_tools,
        exit_reason: None,
        terminal_result: None,
        event_source: Some(source),
        prompt_report: None,
        message_record: None,
        notification_rules: Vec::new(),
        notification_fired: BTreeSet::new(),
        notifier: NotificationService::new(),
        audit: None,
        run_handles: None,
    }
}

async fn collect_stream(mut stream: QueryStream<'_>) -> Result<Vec<StreamEvent>, EngineError> {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        let (event, _usage) = item?;
        events.push(event);
    }
    Ok(events)
}

fn saw_completed(events: &[StreamEvent], tool_name: &str, output: &str, terminal: bool) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            StreamEvent::ToolExecutionCompleted {
                tool_name: name,
                output: got_output,
                is_terminal,
                ..
            } if name == tool_name && got_output == output && *is_terminal == terminal
        )
    })
}

fn transcript_has_tool_result(
    messages: &[Message],
    tool_use_id: &str,
    content: &str,
    is_terminal: bool,
) -> bool {
    messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolResult {
                    tool_use_id: got_id,
                    content: got_content,
                    is_terminal: got_terminal,
                    ..
                } if got_id.as_str() == tool_use_id
                    && got_content == content
                    && *got_terminal == is_terminal
            )
        })
    })
}

#[tokio::test]
async fn run_query_dispatches_non_terminal_tool_and_appends_result_message() {
    let (read_file, read_calls) = canned_tool(ToolName::ReadFile, false, ToolResult::ok("read ok"));
    let (submit_root, submit_calls) =
        canned_tool(ToolName::SubmitRootOutcome, true, ToolResult::ok("done"));
    let source = Arc::new(ScriptedSource::new(vec![
        tool_use_turn("toolu_read", "read_file", json!({"path": "README.md"})),
        tool_use_turn("toolu_stop", "submit_root_outcome", json!({"summary": "done"})),
    ]));
    let mut ctx = ctx(
        source,
        registry(vec![read_file, submit_root]),
        BTreeSet::from([ToolName::SubmitRootOutcome]),
    );
    let mut messages = vec![Message::from_user_text("start")];

    let events = collect_stream(run_query(&mut ctx, &mut messages))
        .await
        .expect("query drains");

    assert_eq!(read_calls.load(Ordering::SeqCst), 1);
    assert_eq!(submit_calls.load(Ordering::SeqCst), 1);
    assert!(saw_completed(&events, "read_file", "read ok", false));
    assert!(transcript_has_tool_result(
        &messages,
        "toolu_read",
        "read ok",
        false
    ));
    assert_eq!(ctx.exit_reason, Some(QueryExitReason::ToolStop));
}

#[tokio::test]
async fn run_query_sets_tool_stop_after_terminal_success() {
    let (submit_root, calls) =
        canned_tool(ToolName::SubmitRootOutcome, true, ToolResult::ok("done"));
    let source = Arc::new(ScriptedSource::new(vec![tool_use_turn(
        "toolu_stop",
        "submit_root_outcome",
        json!({}),
    )]));
    let mut ctx = ctx(
        source,
        registry(vec![submit_root]),
        BTreeSet::from([ToolName::SubmitRootOutcome]),
    );
    let mut messages = vec![Message::from_user_text("start")];

    let events = collect_stream(run_query(&mut ctx, &mut messages))
        .await
        .expect("query drains");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(ctx.exit_reason, Some(QueryExitReason::ToolStop));
    assert!(ctx
        .terminal_result
        .as_ref()
        .is_some_and(|result| result.is_terminal));
    assert!(saw_completed(
        &events,
        "submit_root_outcome",
        "done",
        true
    ));
    assert!(transcript_has_tool_result(
        &messages,
        "toolu_stop",
        "done",
        true
    ));
}

#[tokio::test]
async fn run_query_terminal_tool_error_does_not_set_tool_stop() {
    let (submit_root, calls) =
        canned_tool(ToolName::SubmitRootOutcome, true, ToolResult::error("validation failed"));
    let source = Arc::new(ScriptedSource::new(vec![tool_use_turn(
        "toolu_stop",
        "submit_root_outcome",
        json!({}),
    )]));
    let mut ctx = ctx(
        source,
        registry(vec![submit_root]),
        BTreeSet::from([ToolName::SubmitRootOutcome]),
    );
    let mut messages = vec![Message::from_user_text("start")];

    let mut stream = run_query(&mut ctx, &mut messages);
    let events = run_until(&mut stream, |event| {
        matches!(
            event,
            StreamEvent::ToolExecutionCompleted { tool_name, .. }
                if tool_name == "submit_root_outcome"
        )
    })
    .await;
    drop(stream);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(saw_completed(
        &events,
        "submit_root_outcome",
        "validation failed",
        false
    ));
    assert_eq!(ctx.exit_reason, None);
    assert!(ctx.terminal_result.is_none());
}

#[tokio::test]
async fn run_query_errors_when_provider_stream_has_no_assistant_completion() {
    let source = Arc::new(ScriptedSource::new(vec![Vec::new()]));
    let mut ctx = ctx(source, ToolRegistry::new(), BTreeSet::new());
    let mut messages = vec![Message::from_user_text("start")];

    let mut stream = run_query(&mut ctx, &mut messages);
    let err = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("missing completion errors");
    drop(stream);

    assert!(
        err.to_string()
            .contains("provider stream ended without assistant completion"),
        "{err}"
    );
}

#[tokio::test]
async fn run_query_records_prompt_report_for_request_assistant_and_tool_results() {
    let (submit_root, _calls) =
        canned_tool(ToolName::SubmitRootOutcome, true, ToolResult::ok("done"));
    let source = Arc::new(ScriptedSource::new(vec![tool_use_turn(
        "toolu_stop",
        "submit_root_outcome",
        json!({}),
    )]));
    let mut ctx = ctx(
        source,
        registry(vec![submit_root]),
        BTreeSet::from([ToolName::SubmitRootOutcome]),
    );
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("prompt.jsonl");
    ctx.prompt_report = Some(PromptReportRecorder::new(
        &path,
        ctx.agent_run_id.clone(),
        "root",
        "test-model",
    ));
    let mut messages = vec![Message::from_user_text("start")];

    let _events = collect_stream(run_query(&mut ctx, &mut messages))
        .await
        .expect("query drains");

    let raw = tokio::fs::read_to_string(path).await.expect("read prompt report");
    let lines: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid json"))
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["event"], json!("llm_request"));
    assert_eq!(lines[1]["event"], json!("assistant"));
    assert_eq!(lines[2]["event"], json!("tool_results"));
    assert_eq!(lines[0]["seq"], json!(1));
    assert_eq!(lines[1]["seq"], json!(1));
    assert_eq!(lines[2]["seq"], json!(1));
}

#[tokio::test]
async fn run_query_appends_system_notifications_before_next_provider_request() {
    let source = Arc::new(RecordingSource::new(vec![eos_testkit::text_turn("noted")]));
    let mut ctx = ctx(
        source.clone(),
        ToolRegistry::new(),
        BTreeSet::from([ToolName::SubmitRootOutcome]),
    );
    ctx.notifier
        .notify_system(SystemNotification {
            event: "cmd_1".to_owned(),
            message: "[BACKGROUND COMPLETED] cmd_1".to_owned(),
        })
        .await
        .expect("notification queued");
    let mut messages = vec![Message::from_user_text("start")];

    let mut stream = run_query(&mut ctx, &mut messages);
    let events = run_until(&mut stream, |event| {
        matches!(event, StreamEvent::AssistantMessageComplete { .. })
    })
    .await;
    drop(stream);

    assert!(events.iter().any(|event| {
        matches!(event, StreamEvent::SystemNotification { text, .. }
            if text == "[BACKGROUND COMPLETED] cmd_1")
    }));
    let requests = source.requests().await;
    let first = requests.first().expect("first provider request");
    assert!(
        first.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::SystemNotification { text }
                    if text == "[BACKGROUND COMPLETED] cmd_1")
            })
        }),
        "notification must be appended before the provider request is built"
    );
}

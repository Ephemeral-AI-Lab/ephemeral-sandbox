//! Public fixture-contract tests for `eos-testkit`.
#![allow(clippy::expect_used)]

use eos_engine::{AgentRunStreamEvent, EngineError, ProviderStreamSource};
use eos_llm_client::{ContentBlock, LlmRequest, Message, ToolSpec};
use eos_sandbox_port::{DaemonOp, SandboxTransport};
use eos_testkit::{
    factory_by_agent, run_until, text_turn, tool_use_turn, FakeTransport, ScriptedSource,
};
use eos_types::{
    AgentRunId, AgentRunRecordDir, AgentRunRecordTarget, AgentRunRuntimeSnapshot, JsonObject,
    RequestId, SandboxId, StartAgentLoopRequest, TaskAgentRunKind, TaskId,
};
use futures::StreamExt;
use serde_json::json;

fn request() -> LlmRequest {
    LlmRequest::builder("test-model")
        .message(Message::from_user_text("start"))
        .build()
}

fn request_with_tool(tool_name: &str) -> LlmRequest {
    LlmRequest::builder("test-model")
        .message(Message::from_user_text("start"))
        .tools(vec![ToolSpec::new(
            tool_name,
            "test tool",
            JsonObject::new(),
            None,
        )])
        .build()
}

fn start_request() -> StartAgentLoopRequest {
    let agent_run_id = AgentRunId::new_v4();
    let request_id = RequestId::new_v4();
    let task_id = TaskId::new_v4();
    StartAgentLoopRequest {
        record_target: AgentRunRecordTarget {
            request_id,
            agent_run_id,
            task_id,
            task_agent_run_kind: TaskAgentRunKind::Root,
            record_dir: AgentRunRecordDir::new(
                "requests/test-request/root-task-test-task/agent-run-test-run",
            ),
        },
        initial_messages: Vec::new(),
        model_key: "test-model".to_owned(),
        max_completion_tokens: 1,
        tool_call_limit: 1,
    }
}

fn runtime_snapshot(agent_name: &str) -> AgentRunRuntimeSnapshot {
    AgentRunRuntimeSnapshot {
        agent_run_id: AgentRunId::new_v4(),
        agent_name: agent_name.to_owned(),
        request_id: None,
        task_id: None,
        workflow_id: None,
        iteration_id: None,
        attempt_id: None,
        sandbox_id: None,
        workspace_root: String::new(),
        is_isolated_workspace_mode: false,
    }
}

async fn collect_source(source: &dyn ProviderStreamSource) -> Vec<AgentRunStreamEvent> {
    collect_source_with_request(source, &request()).await
}

async fn collect_source_with_request(
    source: &dyn ProviderStreamSource,
    request: &LlmRequest,
) -> Vec<AgentRunStreamEvent> {
    let mut stream = source.stream(request).await.expect("stream opens");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("event"));
    }
    events
}

fn complete_text(events: &[AgentRunStreamEvent]) -> String {
    match events.last() {
        Some(AgentRunStreamEvent::AssistantMessageComplete { payload, .. }) => {
            payload.message.assistant_text()
        }
        other => panic!("expected assistant completion, got {other:?}"),
    }
}

#[tokio::test]
async fn scripted_source_replays_turns_in_order() {
    let source = ScriptedSource::new(vec![text_turn("first"), text_turn("second")]);

    let first = collect_source(&source).await;
    let second = collect_source(&source).await;

    assert_eq!(complete_text(&first), "first");
    assert_eq!(complete_text(&second), "second");
}

#[tokio::test]
async fn scripted_source_empty_nonblocking_returns_empty_stream() {
    let source = ScriptedSource::new(Vec::new());

    let mut stream = source.stream(&request()).await.expect("stream opens");

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn factory_by_agent_routes_by_agent_name() {
    let factory = factory_by_agent(vec![
        ("root", vec![text_turn("root turn")]),
        ("advisor", vec![text_turn("advisor turn")]),
    ]);
    let start_request = start_request();
    let root_source = factory(&start_request, &runtime_snapshot("root"));
    let advisor_source = factory(&start_request, &runtime_snapshot("advisor"));
    let root_events = collect_source_with_request(
        &*root_source,
        &request_with_tool("submit_root_task_outcome"),
    )
    .await;
    let advisor_events = collect_source_with_request(
        &*advisor_source,
        &request_with_tool("submit_advisor_outcome"),
    )
    .await;

    assert_eq!(complete_text(&root_events), "root turn");
    assert_eq!(complete_text(&advisor_events), "advisor turn");
}

#[test]
fn tool_use_turn_drops_non_object_input_to_empty_object() {
    let events = tool_use_turn("toolu_1", "read_file", json!("not an object"));

    let input = match events.as_slice() {
        [AgentRunStreamEvent::AssistantMessageComplete { payload, .. }] => {
            match payload.message.content.as_slice() {
                [ContentBlock::ToolUse { input, .. }] => input,
                other => panic!("expected tool use block, got {other:?}"),
            }
        }
        other => panic!("expected one assistant completion, got {other:?}"),
    };
    assert!(input.is_empty());
}

#[tokio::test]
async fn run_until_stops_inclusively_at_matching_event() {
    let events = vec![
        Ok(AgentRunStreamEvent::AssistantTextDelta {
            agent_name: String::new(),
            agent_run_id: None,
            text: "one".to_owned(),
        }),
        Ok(AgentRunStreamEvent::AssistantTextDelta {
            agent_name: String::new(),
            agent_run_id: None,
            text: "two".to_owned(),
        }),
        Ok(AgentRunStreamEvent::AssistantTextDelta {
            agent_name: String::new(),
            agent_run_id: None,
            text: "three".to_owned(),
        }),
    ];
    let mut stream = Box::pin(futures::stream::iter::<Vec<Result<_, EngineError>>>(events));

    let collected = run_until(
        &mut stream,
        |event| matches!(event, AgentRunStreamEvent::AssistantTextDelta { text, .. } if text == "two"),
    )
    .await;

    assert_eq!(collected.len(), 2);
    assert!(matches!(
        collected.last(),
        Some(AgentRunStreamEvent::AssistantTextDelta { text, .. }) if text == "two"
    ));
}

#[tokio::test]
async fn fake_transport_returns_empty_payload() {
    let payload = FakeTransport
        .call(
            &SandboxId::new_v4(),
            DaemonOp::ReadFile,
            JsonObject::new(),
            1,
        )
        .await
        .expect("fake transport call");

    assert!(payload.is_empty());
}

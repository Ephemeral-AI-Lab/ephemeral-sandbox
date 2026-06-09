#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Debug;
use std::path::Path;
use std::str::FromStr;

use eos_engine::run_output::{
    AgentRunRecordError, AgentRunRecordFinishStatus, AgentRunRecordHandle, AgentRunRecordStore,
};
use eos_types::{
    format_record_dir, AgentRunId, AgentRunRecordDir, AgentRunRecordIndex, AgentRunRecordTarget,
    AttemptId, ContentBlock, IterationId, Message, MessageRole, ParentedAgentRunKind, RequestId,
    TaskAgentRunKind, TaskId, ToolUseId, WorkflowCoordinates, WorkflowId, WorkflowTaskRole,
};
use serde_json::{json, Value};

fn id<T>(value: &str) -> T
where
    T: FromStr,
    T::Err: Debug,
{
    value.parse().expect("valid id")
}

fn ids() -> (RequestId, TaskId, AgentRunId) {
    (id("req-1"), id("task-1"), id("run-1"))
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn record_target(
    request_id: &RequestId,
    task_id: &TaskId,
    agent_run_id: &AgentRunId,
    kind: TaskAgentRunKind,
    parent_record_dir: Option<AgentRunRecordDir>,
) -> AgentRunRecordTarget {
    let record_dir = format_record_dir(&AgentRunRecordIndex {
        request_id: request_id.clone(),
        agent_run_id: agent_run_id.clone(),
        task_id: task_id.clone(),
        kind: kind.clone(),
        parent_record_dir,
    });
    AgentRunRecordTarget {
        request_id: request_id.clone(),
        agent_run_id: agent_run_id.clone(),
        task_id: task_id.clone(),
        task_agent_run_kind: kind,
        record_dir,
    }
}

async fn start_record(
    records: &AgentRunRecordStore,
    target: &AgentRunRecordTarget,
    agent_name: &str,
    system_prompt: &str,
    initial_messages: &[Message],
) -> AgentRunRecordHandle {
    records
        .start_agent_run_at(target, agent_name, system_prompt, initial_messages)
        .await
        .expect("start record")
}

async fn start_root(
    records: &AgentRunRecordStore,
    request_id: &RequestId,
    task_id: &TaskId,
    agent_run_id: &AgentRunId,
) -> AgentRunRecordHandle {
    let target = record_target(
        request_id,
        task_id,
        agent_run_id,
        TaskAgentRunKind::Root,
        None,
    );
    start_record(records, &target, "root", "system", &[]).await
}

fn workflow_kind(
    workflow_id: &WorkflowId,
    iteration_id: &IterationId,
    attempt_id: &AttemptId,
    role: WorkflowTaskRole,
) -> TaskAgentRunKind {
    TaskAgentRunKind::Workflow {
        workflow: WorkflowCoordinates {
            workflow_id: workflow_id.clone(),
            iteration_id: iteration_id.clone(),
            attempt_id: attempt_id.clone(),
        },
        role,
    }
}

fn parented_kind(parent_agent_run_id: &AgentRunId, kind: ParentedAgentRunKind) -> TaskAgentRunKind {
    TaskAgentRunKind::Parented {
        parent_agent_run_id: parent_agent_run_id.clone(),
        kind,
    }
}

fn assert_unsafe_segment<T>(
    result: std::result::Result<T, AgentRunRecordError>,
    expected_field: &str,
    expected_value: &str,
) where
    T: Debug,
{
    match result {
        Err(AgentRunRecordError::UnsafeSegment { field, value }) => {
            assert_eq!(field, expected_field);
            assert_eq!(value, expected_value);
        }
        other => panic!("expected unsafe segment for {expected_field}, got {other:?}"),
    }
}

#[tokio::test]
async fn root_start_writes_initial_messages_and_events() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let target = record_target(
        &request_id,
        &task_id,
        &agent_run_id,
        TaskAgentRunKind::Root,
        None,
    );

    let handle = start_record(
        &records,
        &target,
        "root",
        "system prompt",
        &[Message::from_user_text("hello")],
    )
    .await;

    let raw = tokio::fs::read_to_string(handle.record_dir().join("messages.jsonl"))
        .await
        .unwrap();
    let rows: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["type"], json!("initial_message"));
    assert_eq!(rows[0]["request_id"], json!("req-1"));
    assert_eq!(rows[0]["task_id"], json!("task-1"));
    assert_eq!(rows[0]["agent_run_id"], json!("run-1"));
    assert_eq!(rows[0]["role"], json!("system"));
    assert_eq!(rows[0]["content"][0]["text"], json!("system prompt"));
    assert_eq!(rows[1]["role"], json!("user"));
    assert!(rows[0].get("turn").is_none());
    assert!(rows[0].get("initial_index").is_none());

    let events = handle.read_record_events(0).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].request_id, "req-1");
    assert_eq!(events[0].task_id, "task-1");
    assert_eq!(events[0].agent_run_id, "run-1");
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].kind, "node_started");
    assert_eq!(events[0].payload["type"], json!("root_agent"));
    assert_eq!(events[0].payload["agent"], json!("root"));
    assert_eq!(events[1].kind, "messages_initialized");
    assert_eq!(events[1].payload["count"], json!(2));
    assert!(events[1].payload["messages_end_byte"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn workflow_task_records_use_role_layout_and_payload() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let request_id: RequestId = id("req-workflow");
    let root_task_id: TaskId = id("task-root");
    let root_run_id: AgentRunId = id("run-root");
    let root_handle = start_root(&records, &request_id, &root_task_id, &root_run_id).await;

    let workflow_id: WorkflowId = id("wf-1");
    let iteration_id: IterationId = id("it-1");
    let attempt_id: AttemptId = id("att-1");
    let cases = [
        (
            WorkflowTaskRole::Planner,
            "workflow_planner",
            "planner",
            "planner-task-task-plan",
            "task-plan",
            "run-plan",
        ),
        (
            WorkflowTaskRole::Worker,
            "workflow_worker",
            "worker",
            "worker-task-task-work",
            "task-work",
            "run-work",
        ),
    ];

    for (role, node_type, role_label, task_segment, task_value, run_value) in cases {
        let task_id: TaskId = id(task_value);
        let agent_run_id: AgentRunId = id(run_value);
        let target = record_target(
            &request_id,
            &task_id,
            &agent_run_id,
            workflow_kind(&workflow_id, &iteration_id, &attempt_id, role),
            None,
        );

        let handle = start_record(&records, &target, "worker", "workflow system", &[]).await;

        let relative = slash(handle.record_dir().strip_prefix(dir.path()).unwrap());
        assert_eq!(
            relative,
            format!(
                concat!(
                    "requests/req-workflow/",
                    "workflows/workflow-wf-1/iteration-it-1/attempt-att-1/",
                    "{}/agent-run-{}"
                ),
                task_segment, run_value
            )
        );

        let events = handle.read_record_events(0).await.unwrap();
        assert_eq!(events[0].kind, "node_started");
        assert_eq!(events[0].payload["type"], json!(node_type));
        assert_eq!(events[0].payload["agent_run_id"], json!(run_value));
        assert_eq!(events[0].payload["task_id"], json!(task_value));
        assert_eq!(events[0].payload["workflow_id"], json!("wf-1"));
        assert_eq!(events[0].payload["iteration_id"], json!("it-1"));
        assert_eq!(events[0].payload["attempt_id"], json!("att-1"));
        assert_eq!(events[0].payload["role"], json!(role_label));
    }

    let root_events = root_handle.read_record_events(2).await.unwrap();
    assert!(root_events
        .iter()
        .all(|event| event.kind != "child_created"));
}

#[tokio::test]
async fn later_messages_append_byte_ranges_and_content_types() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = start_root(&records, &request_id, &task_id, &agent_run_id).await;

    let empty = handle.append_messages(&[]).await.unwrap();
    assert_eq!(empty.count, 0);
    assert_eq!(empty.start_byte, empty.end_byte);
    assert!(handle.read_record_events(2).await.unwrap().is_empty());

    let range = handle
        .append_messages(&[
            Message {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "answer".to_owned(),
                    },
                    ContentBlock::ToolUse {
                        tool_use_id: id::<ToolUseId>("toolu-1"),
                        name: "read_file".to_owned(),
                        input: json!({"path": "Cargo.toml"}).as_object().unwrap().clone(),
                    },
                    ContentBlock::Reasoning {
                        text: "thinking".to_owned(),
                    },
                ],
            },
            Message {
                role: MessageRole::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: id::<ToolUseId>("toolu-1"),
                        content: "done".to_owned(),
                        is_error: false,
                        metadata: json!({"bytes": 12}).as_object().unwrap().clone(),
                        is_terminal: false,
                    },
                    ContentBlock::SystemNotification {
                        text: "remember".to_owned(),
                    },
                    ContentBlock::Text {
                        text: "follow-up".to_owned(),
                    },
                ],
            },
        ])
        .await
        .unwrap();
    assert_eq!(range.count, 2);

    let events = handle.read_record_events(2).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "messages_appended");
    assert_eq!(
        events[0].payload["message_types"],
        json!([
            "reasoning",
            "system_notification",
            "text",
            "tool_result",
            "tool_use"
        ])
    );
    assert!(events[0].payload.get("content").is_none());

    let tail = handle.read_messages(range.start_byte).await.unwrap();
    let rows: Vec<Value> = String::from_utf8(tail.bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["type"], json!("message"));
    assert_eq!(rows[0]["role"], json!("assistant"));
    assert_eq!(rows[0]["content"][0]["type"], json!("text"));
    assert_eq!(rows[0]["content"][1]["type"], json!("tool_use"));
    assert_eq!(rows[0]["content"][2]["type"], json!("reasoning"));
    assert_eq!(rows[1]["role"], json!("user"));
    assert_eq!(rows[1]["content"][0]["type"], json!("tool_result"));
    assert_eq!(rows[1]["content"][1]["type"], json!("system_notification"));
    assert_eq!(rows[1]["content"][2]["type"], json!("text"));
    assert_eq!(tail.next_byte_offset, range.end_byte);
}

#[tokio::test]
async fn finish_and_tail_reads_use_offsets_and_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = start_root(&records, &request_id, &task_id, &agent_run_id).await;
    let initialized = handle.read_record_events(0).await.unwrap();
    let messages_end = initialized[1].payload["messages_end_byte"]
        .as_u64()
        .unwrap();

    let eof = handle.read_messages(messages_end).await.unwrap();
    assert!(eof.bytes.is_empty());
    assert_eq!(eof.next_byte_offset, messages_end);

    assert!(matches!(
        handle.read_messages(messages_end + 1).await,
        Err(AgentRunRecordError::OffsetOutOfRange {
            offset,
            len
        }) if offset == messages_end + 1 && len == messages_end
    ));

    handle
        .finish(AgentRunRecordFinishStatus::Completed)
        .await
        .unwrap();
    handle
        .finish(AgentRunRecordFinishStatus::Failed)
        .await
        .unwrap();

    let events = handle.read_record_events(2).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 3);
    assert_eq!(events[0].kind, "node_finished");
    assert_eq!(events[0].payload["status"], json!("completed"));
    assert_eq!(events[1].seq, 4);
    assert_eq!(events[1].payload["status"], json!("failed"));
    assert!(handle.read_record_events(4).await.unwrap().is_empty());
}

#[tokio::test]
async fn parented_records_use_resolved_or_request_rooted_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let (request_id, task_id, parent_id) = ids();
    let parent = start_root(&records, &request_id, &task_id, &parent_id).await;

    let child_id: AgentRunId = id("child-run");
    let child_task_id: TaskId = id("child-task");
    let child_target = record_target(
        &request_id,
        &child_task_id,
        &child_id,
        parented_kind(&parent_id, ParentedAgentRunKind::Subagent),
        None,
    );
    let child = start_record(&records, &child_target, "subagent", "system", &[]).await;
    assert_eq!(
        slash(child.record_dir().strip_prefix(dir.path()).unwrap()),
        "requests/req-1/subagents/subagent-run-child-run"
    );

    let parent_record_dir =
        AgentRunRecordDir::new(slash(parent.record_dir().strip_prefix(dir.path()).unwrap()));
    let nested_id: AgentRunId = id("nested-child");
    let nested_task_id: TaskId = id("nested-task");
    let nested_target = record_target(
        &request_id,
        &nested_task_id,
        &nested_id,
        parented_kind(&parent_id, ParentedAgentRunKind::Subagent),
        Some(parent_record_dir.clone()),
    );
    let nested = start_record(&records, &nested_target, "subagent", "system", &[]).await;
    assert_eq!(
        slash(nested.record_dir().strip_prefix(dir.path()).unwrap()),
        "requests/req-1/root-task-task-1/agent-run-run-1/subagents/subagent-run-nested-child"
    );
    assert!(!records
        .read_record_events_at(&nested_target.record_dir, 0)
        .await
        .unwrap()
        .is_empty());
    assert!(parent
        .read_record_events(2)
        .await
        .unwrap()
        .iter()
        .all(|event| event.kind != "child_created"));
}

#[tokio::test]
async fn advisor_records_parent_payload() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let (request_id, _task_id, parent_id) = ids();
    let advisor_id: AgentRunId = id("advisor-child");
    let advisor_task_id: TaskId = id("advisor-task");
    let advisor_target = record_target(
        &request_id,
        &advisor_task_id,
        &advisor_id,
        parented_kind(&parent_id, ParentedAgentRunKind::Advisor),
        None,
    );

    let advisor = start_record(&records, &advisor_target, "advisor", "advisor system", &[]).await;

    assert_eq!(
        slash(advisor.record_dir().strip_prefix(dir.path()).unwrap()),
        "requests/req-1/advisors/advisor-run-advisor-child"
    );
    assert!(advisor
        .read_record_events(0)
        .await
        .unwrap()
        .iter()
        .any(|event| event.payload["parent_agent_run_id"] == json!("run-1")));
}

#[tokio::test]
async fn unknown_agent_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let missing = AgentRunRecordDir::new("requests/req-missing/root-task-task/agent-run-missing");

    assert!(matches!(
        records.read_record_events_at(&missing, 0).await,
        Err(AgentRunRecordError::NotFound(_))
    ));
}

#[tokio::test]
async fn unsafe_path_segments_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordStore::new(dir.path());
    let request_id: RequestId = id("req-safe");
    let task_id: TaskId = id("task-safe");
    let agent_run_id: AgentRunId = id("run-safe");

    let unsafe_request: RequestId = id("req/escape");
    let target = record_target(
        &unsafe_request,
        &task_id,
        &agent_run_id,
        TaskAgentRunKind::Root,
        None,
    );
    assert_unsafe_segment(
        records
            .start_agent_run_at(&target, "root", "system", &[])
            .await,
        "request_id",
        "req/escape",
    );

    let unsafe_task: TaskId = id("../task");
    let target = record_target(
        &request_id,
        &unsafe_task,
        &agent_run_id,
        TaskAgentRunKind::Root,
        None,
    );
    assert_unsafe_segment(
        records
            .start_agent_run_at(&target, "root", "system", &[])
            .await,
        "task_id",
        "../task",
    );

    let unsafe_agent_run: AgentRunId = id(r"run\escape");
    let target = record_target(
        &request_id,
        &task_id,
        &unsafe_agent_run,
        TaskAgentRunKind::Root,
        None,
    );
    assert_unsafe_segment(
        records
            .start_agent_run_at(&target, "root", "system", &[])
            .await,
        "agent-run",
        r"run\escape",
    );

    let unsafe_workflow: WorkflowId = id("wf/escape");
    let target = record_target(
        &request_id,
        &task_id,
        &agent_run_id,
        workflow_kind(
            &unsafe_workflow,
            &id("it-safe"),
            &id("att-safe"),
            WorkflowTaskRole::Planner,
        ),
        None,
    );
    assert_unsafe_segment(
        records
            .start_agent_run_at(&target, "planner", "system", &[])
            .await,
        "workflow",
        "wf/escape",
    );

    let unsafe_parent: AgentRunId = id("parent/escape");
    let target = record_target(
        &request_id,
        &task_id,
        &agent_run_id,
        parented_kind(&unsafe_parent, ParentedAgentRunKind::Subagent),
        None,
    );
    assert_unsafe_segment(
        records
            .start_agent_run_at(&target, "subagent", "system", &[])
            .await,
        "agent_run_id",
        "parent/escape",
    );

    let mut target = record_target(
        &request_id,
        &task_id,
        &agent_run_id,
        TaskAgentRunKind::Root,
        None,
    );
    target.record_dir = AgentRunRecordDir::new("requests/req-safe/../escape");
    assert_unsafe_segment(
        records
            .start_agent_run_at(&target, "root", "system", &[])
            .await,
        "record_dir",
        "..",
    );
}

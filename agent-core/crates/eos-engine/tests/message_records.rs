#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Debug;
use std::path::Path;
use std::str::FromStr;

use eos_engine::records::{
    AgentRunRecordError, AgentRunRecordHandle, AgentRunRecordKind, AgentRunRecordStart,
    AgentRunRecordWriter, NodeFinishStatus, WorkflowTaskRole,
};
use eos_types::{
    AgentRunId, AgentRunRecordDir, AttemptId, ContentBlock, IterationId, Message, MessageRole,
    RequestId, TaskId, ToolUseId, WorkflowId,
};
use serde_json::{json, Value};

fn ids() -> (RequestId, TaskId, AgentRunId) {
    (
        "req-1".parse().unwrap(),
        "task-1".parse().unwrap(),
        "run-1".parse().unwrap(),
    )
}

fn id<T>(value: &str) -> T
where
    T: FromStr,
    T::Err: Debug,
{
    value.parse().expect("valid id")
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

async fn start_root(
    records: &AgentRunRecordWriter,
    request_id: &RequestId,
    task_id: &TaskId,
    agent_run_id: &AgentRunId,
) -> AgentRunRecordHandle {
    records
        .start_agent_run(AgentRunRecordStart {
            request_id,
            task_id: Some(task_id),
            agent_run_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .expect("start root")
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
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &agent_run_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system prompt",
            initial_messages: &[Message::from_user_text("hello")],
        })
        .await
        .expect("start");

    let raw = tokio::fs::read_to_string(handle.node_dir().join("messages.jsonl"))
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
    assert_eq!(rows[1]["request_id"], json!("req-1"));
    assert_eq!(rows[1]["task_id"], json!("task-1"));
    assert_eq!(rows[1]["agent_run_id"], json!("run-1"));
    assert_eq!(rows[1]["role"], json!("user"));
    assert!(rows[0].get("turn").is_none());
    assert!(rows[0].get("initial_index").is_none());

    let events = handle.read_events(0).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].request_id, "req-1");
    assert_eq!(events[0].task_id, "task-1");
    assert_eq!(events[0].agent_run_id, "run-1");
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].kind, "node_started");
    assert_eq!(events[1].request_id, "req-1");
    assert_eq!(events[1].task_id, "task-1");
    assert_eq!(events[1].agent_run_id, "run-1");
    assert_eq!(events[1].kind, "messages_initialized");
    assert_eq!(events[1].payload["count"], json!(2));
    assert!(events[1].payload["messages_end_byte"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn workflow_task_records_use_role_layout_and_payload() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
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
            WorkflowTaskRole::Generator,
            "workflow_generator",
            "generator",
            "generator-task-task-gen",
            "task-gen",
            "run-gen",
        ),
        (
            WorkflowTaskRole::Reducer,
            "workflow_reducer",
            "reducer",
            "reducer-task-task-reduce",
            "task-reduce",
            "run-reduce",
        ),
    ];

    for (role, node_type, role_label, task_segment, task_value, run_value) in cases {
        let task_id: TaskId = id(task_value);
        let agent_run_id: AgentRunId = id(run_value);
        let kind = AgentRunRecordKind::WorkflowTask {
            workflow_id: workflow_id.clone(),
            iteration_id: iteration_id.clone(),
            attempt_id: attempt_id.clone(),
            role,
        };

        let handle = records
            .start_agent_run(AgentRunRecordStart {
                request_id: &request_id,
                task_id: Some(&task_id),
                agent_run_id: &agent_run_id,
                agent_name: "worker",
                kind: &kind,
                system_prompt: "workflow system",
                initial_messages: &[],
            })
            .await
            .expect("start workflow task");

        let relative = slash(handle.node_dir().strip_prefix(dir.path()).unwrap());
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

        let events = handle.read_events(0).await.unwrap();
        assert_eq!(events[0].kind, "node_started");
        assert_eq!(events[0].payload["type"], json!(node_type));
        assert_eq!(events[0].payload["agent_run_id"], json!(run_value));
        assert_eq!(events[0].payload["task_id"], json!(task_value));
        assert_eq!(events[0].payload["workflow_id"], json!("wf-1"));
        assert_eq!(events[0].payload["iteration_id"], json!("it-1"));
        assert_eq!(events[0].payload["attempt_id"], json!("att-1"));
        assert_eq!(events[0].payload["role"], json!(role_label));
    }

    let root_events = root_handle.read_events(2).await.unwrap();
    assert!(root_events
        .iter()
        .all(|event| event.kind != "child_created"));
}

#[tokio::test]
async fn later_messages_append_byte_ranges_without_event_content() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &agent_run_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();

    let range = handle
        .append_messages(&[Message {
            role: MessageRole::User,
            content: vec![ContentBlock::SystemNotification {
                text: "remember".to_owned(),
            }],
        }])
        .await
        .unwrap();
    assert_eq!(range.count, 1);
    assert!(range.end_byte > range.start_byte);

    let events = handle.read_events(2).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "messages_appended");
    assert_eq!(
        events[0].payload["message_types"],
        json!(["system_notification"])
    );
    assert!(events[0].payload.get("content").is_none());

    let tail = handle.read_messages(range.start_byte).await.unwrap();
    let text = String::from_utf8(tail.bytes).unwrap();
    assert!(text.contains("system_notification"));
    assert_eq!(tail.next_byte_offset, range.end_byte);
}

#[tokio::test]
async fn appended_messages_record_all_content_types_and_empty_append_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &agent_run_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();

    let empty = handle.append_messages(&[]).await.unwrap();
    assert_eq!(empty.count, 0);
    assert_eq!(empty.start_byte, empty.end_byte);
    assert!(handle.read_events(2).await.unwrap().is_empty());

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

    let events = handle.read_events(2).await.unwrap();
    assert_eq!(events.len(), 1);
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
}

#[tokio::test]
async fn finish_appends_terminal_status_events_in_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &agent_run_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();

    handle.finish(NodeFinishStatus::Completed).await.unwrap();
    handle.finish(NodeFinishStatus::Failed).await.unwrap();

    let events = handle.read_events(2).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 3);
    assert_eq!(events[0].kind, "node_finished");
    assert_eq!(events[0].payload["status"], json!("completed"));
    assert_eq!(events[1].seq, 4);
    assert_eq!(events[1].kind, "node_finished");
    assert_eq!(events[1].payload["status"], json!("failed"));
}

#[tokio::test]
async fn read_messages_and_events_honor_tail_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, agent_run_id) = ids();
    let handle = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &agent_run_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();
    let initialized = handle.read_events(0).await.unwrap();
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

    handle.finish(NodeFinishStatus::Completed).await.unwrap();
    let after_init = handle.read_events(2).await.unwrap();
    assert_eq!(after_init.len(), 1);
    assert_eq!(after_init[0].seq, 3);
    assert!(handle.read_events(3).await.unwrap().is_empty());
}

#[tokio::test]
async fn subagent_records_use_request_rooted_layout_without_parent_scan() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, parent_id) = ids();
    let parent = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &parent_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();
    let child_id: AgentRunId = "child-run".parse().unwrap();
    let child_task_id: TaskId = "child-task".parse().unwrap();
    let child = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&child_task_id),
            agent_run_id: &child_id,
            agent_name: "subagent",
            kind: &AgentRunRecordKind::Subagent {
                parent_agent_run_id: parent_id.clone(),
            },
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();

    assert!(child.node_dir().join("messages.jsonl").exists());
    assert_eq!(
        slash(child.node_dir().strip_prefix(dir.path()).unwrap()),
        "requests/req-1/subagents/subagent-run-child-run"
    );
    let parent_events = parent.read_events(0).await.unwrap();
    assert!(parent_events
        .iter()
        .all(|event| event.kind != "child_created"));
}

#[tokio::test]
async fn parented_records_can_start_at_resolved_parent_layout() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, parent_id) = ids();
    let parent = start_root(&records, &request_id, &task_id, &parent_id).await;
    let parent_record_dir =
        AgentRunRecordDir::new(slash(parent.node_dir().strip_prefix(dir.path()).unwrap()));

    let child_id: AgentRunId = "child-run".parse().unwrap();
    let child_task_id: TaskId = "child-task".parse().unwrap();
    let child_record_dir = AgentRunRecordDir::new(format!(
        "{}/subagents/subagent-run-child-run",
        parent_record_dir.as_str()
    ));
    let child = records
        .start_agent_run_at(
            &child_record_dir,
            AgentRunRecordStart {
                request_id: &request_id,
                task_id: Some(&child_task_id),
                agent_run_id: &child_id,
                agent_name: "subagent",
                kind: &AgentRunRecordKind::Subagent {
                    parent_agent_run_id: parent_id,
                },
                system_prompt: "system",
                initial_messages: &[],
            },
        )
        .await
        .unwrap();

    assert_eq!(
        slash(child.node_dir().strip_prefix(dir.path()).unwrap()),
        "requests/req-1/root-task-task-1/agent-run-run-1/subagents/subagent-run-child-run"
    );
    assert!(!records
        .read_events_at(&child_record_dir, 0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn advisor_child_created_records_parent_payload_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, parent_id) = ids();
    let parent = start_root(&records, &request_id, &task_id, &parent_id).await;

    let advisor_id: AgentRunId = id("advisor-child");
    let advisor_task_id: TaskId = id("advisor-task");
    let advisor = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&advisor_task_id),
            agent_run_id: &advisor_id,
            agent_name: "advisor",
            kind: &AgentRunRecordKind::Advisor {
                parent_agent_run_id: parent_id.clone(),
            },
            system_prompt: "advisor system",
            initial_messages: &[],
        })
        .await
        .unwrap();

    let parent_events = parent.read_events(2).await.unwrap();
    assert!(parent_events.is_empty());
    assert!(advisor
        .read_events(0)
        .await
        .unwrap()
        .iter()
        .any(|event| event.payload["parent_agent_run_id"] == json!("run-1")));
}

#[tokio::test]
async fn parented_runs_do_not_use_parents_missing_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let request_id: RequestId = id("req-layout");

    let missing_parent: AgentRunId = id("missing-parent");
    let orphan_id: AgentRunId = id("orphan-subagent");
    let orphan_task_id: TaskId = id("orphan-task");
    let orphan = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&orphan_task_id),
            agent_run_id: &orphan_id,
            agent_name: "subagent",
            kind: &AgentRunRecordKind::Subagent {
                parent_agent_run_id: missing_parent,
            },
            system_prompt: "subagent system",
            initial_messages: &[],
        })
        .await
        .unwrap();
    assert_eq!(
        slash(orphan.node_dir().strip_prefix(dir.path()).unwrap()),
        "requests/req-layout/subagents/subagent-run-orphan-subagent"
    );
    assert!(orphan.read_events(0).await.unwrap().len() >= 2);
}

#[tokio::test]
async fn subagent_and_advisor_records_read_from_handles() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let (request_id, task_id, parent_id) = ids();
    let subagent = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&task_id),
            agent_run_id: &parent_id,
            agent_name: "root",
            kind: &AgentRunRecordKind::Root,
            system_prompt: "system",
            initial_messages: &[],
        })
        .await
        .unwrap();

    let subagent_id: AgentRunId = "subagent-1".parse().unwrap();
    let subagent_task_id: TaskId = "subagent-task-1".parse().unwrap();
    records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&subagent_task_id),
            agent_run_id: &subagent_id,
            agent_name: "subagent",
            kind: &AgentRunRecordKind::Subagent {
                parent_agent_run_id: parent_id.clone(),
            },
            system_prompt: "subagent system",
            initial_messages: &[],
        })
        .await
        .unwrap();
    assert!(!subagent.read_messages(0).await.unwrap().bytes.is_empty());

    let advisor_id: AgentRunId = "advisor-1".parse().unwrap();
    let advisor_task_id: TaskId = "advisor-task-1".parse().unwrap();
    let advisor = records
        .start_agent_run(AgentRunRecordStart {
            request_id: &request_id,
            task_id: Some(&advisor_task_id),
            agent_run_id: &advisor_id,
            agent_name: "advisor",
            kind: &AgentRunRecordKind::Advisor {
                parent_agent_run_id: parent_id,
            },
            system_prompt: "advisor system",
            initial_messages: &[],
        })
        .await
        .unwrap();
    assert!(!advisor.read_events(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn unknown_agent_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let missing = AgentRunRecordDir::new("requests/req-missing/root-task-task/agent-run-missing");

    assert!(matches!(
        records.read_events_at(&missing, 0).await,
        Err(AgentRunRecordError::NotFound(_))
    ));
}

#[tokio::test]
async fn unsafe_path_segments_and_missing_task_ids_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let records = AgentRunRecordWriter::new(dir.path());
    let request_id: RequestId = id("req-safe");
    let task_id: TaskId = id("task-safe");
    let agent_run_id: AgentRunId = id("run-safe");
    let root = AgentRunRecordKind::Root;

    let unsafe_request: RequestId = id("req/escape");
    assert_unsafe_segment(
        records
            .start_agent_run(AgentRunRecordStart {
                request_id: &unsafe_request,
                task_id: Some(&task_id),
                agent_run_id: &agent_run_id,
                agent_name: "root",
                kind: &root,
                system_prompt: "system",
                initial_messages: &[],
            })
            .await,
        "request_id",
        "req/escape",
    );

    let unsafe_task: TaskId = id("../task");
    assert_unsafe_segment(
        records
            .start_agent_run(AgentRunRecordStart {
                request_id: &request_id,
                task_id: Some(&unsafe_task),
                agent_run_id: &agent_run_id,
                agent_name: "root",
                kind: &root,
                system_prompt: "system",
                initial_messages: &[],
            })
            .await,
        "task_id",
        "../task",
    );

    let unsafe_agent_run: AgentRunId = id(r"run\escape");
    assert_unsafe_segment(
        records
            .start_agent_run(AgentRunRecordStart {
                request_id: &request_id,
                task_id: Some(&task_id),
                agent_run_id: &unsafe_agent_run,
                agent_name: "root",
                kind: &root,
                system_prompt: "system",
                initial_messages: &[],
            })
            .await,
        "agent-run",
        r"run\escape",
    );

    assert_unsafe_segment(
        records
            .start_agent_run(AgentRunRecordStart {
                request_id: &request_id,
                task_id: None,
                agent_run_id: &agent_run_id,
                agent_name: "root",
                kind: &root,
                system_prompt: "system",
                initial_messages: &[],
            })
            .await,
        "task_id",
        "",
    );

    let unsafe_workflow: WorkflowId = id("wf/escape");
    let workflow = AgentRunRecordKind::WorkflowTask {
        workflow_id: unsafe_workflow,
        iteration_id: id("it-safe"),
        attempt_id: id("att-safe"),
        role: WorkflowTaskRole::Planner,
    };
    assert_unsafe_segment(
        records
            .start_agent_run(AgentRunRecordStart {
                request_id: &request_id,
                task_id: Some(&task_id),
                agent_run_id: &agent_run_id,
                agent_name: "planner",
                kind: &workflow,
                system_prompt: "system",
                initial_messages: &[],
            })
            .await,
        "workflow",
        "wf/escape",
    );

    let unsafe_parent: AgentRunId = id("parent/escape");
    let subagent = AgentRunRecordKind::Subagent {
        parent_agent_run_id: unsafe_parent,
    };
    assert_unsafe_segment(
        records
            .start_agent_run(AgentRunRecordStart {
                request_id: &request_id,
                task_id: Some(&task_id),
                agent_run_id: &agent_run_id,
                agent_name: "subagent",
                kind: &subagent,
                system_prompt: "system",
                initial_messages: &[],
            })
            .await,
        "agent_run_id",
        "parent/escape",
    );
}

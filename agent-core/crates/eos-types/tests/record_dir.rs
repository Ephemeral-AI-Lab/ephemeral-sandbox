#![allow(clippy::expect_used)]

use eos_types::{
    format_record_dir, AgentRunId, AgentRunRecordDir, AgentRunRecordIndex, AttemptId, IterationId,
    ParentedAgentRunKind, RequestId, TaskAgentRunKind, TaskId, WorkflowCoordinates, WorkflowId,
    WorkflowTaskRole,
};

fn id<T>(value: &str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug,
{
    value.parse().expect("valid id")
}

fn index(
    request_id: &RequestId,
    task_id: &TaskId,
    agent_run_id: &AgentRunId,
    kind: TaskAgentRunKind,
    parent_record_dir: Option<AgentRunRecordDir>,
) -> AgentRunRecordIndex {
    AgentRunRecordIndex {
        request_id: request_id.clone(),
        agent_run_id: agent_run_id.clone(),
        task_id: task_id.clone(),
        kind,
        parent_record_dir,
    }
}

#[test]
fn root_record_dir_is_request_rooted() {
    let request_id: RequestId = id("req-1");
    let task_id: TaskId = id("task-1");
    let agent_run_id: AgentRunId = id("run-1");

    let dir = format_record_dir(&index(
        &request_id,
        &task_id,
        &agent_run_id,
        TaskAgentRunKind::Root,
        None,
    ));

    assert_eq!(
        dir.as_str(),
        "requests/req-1/root-task-task-1/agent-run-run-1"
    );
}

#[test]
fn workflow_record_dirs_use_role_task_segments() {
    let request_id: RequestId = id("req-workflow");
    let workflow = WorkflowCoordinates {
        workflow_id: id::<WorkflowId>("wf-1"),
        iteration_id: id::<IterationId>("it-1"),
        attempt_id: id::<AttemptId>("att-1"),
    };
    let cases = [
        (
            WorkflowTaskRole::Planner,
            "task-plan",
            "run-plan",
            "planner-task-task-plan",
        ),
        (
            WorkflowTaskRole::Worker,
            "task-work",
            "run-work",
            "worker-task-task-work",
        ),
    ];

    for (role, task_id, agent_run_id, task_segment) in cases {
        let task_id = id::<TaskId>(task_id);
        let agent_run_id = id::<AgentRunId>(agent_run_id);
        let dir = format_record_dir(&index(
            &request_id,
            &task_id,
            &agent_run_id,
            TaskAgentRunKind::Workflow {
                workflow: workflow.clone(),
                role,
            },
            None,
        ));

        assert_eq!(
            dir.as_str(),
            format!(
                "requests/req-workflow/workflows/workflow-wf-1/iteration-it-1/attempt-att-1/{task_segment}/agent-run-{}",
                agent_run_id.as_str()
            )
        );
    }
}

#[test]
fn parented_record_dirs_use_request_or_parent_root() {
    let request_id: RequestId = id("req-1");
    let task_id: TaskId = id("child-task");
    let parent_agent_run_id: AgentRunId = id("parent-run");
    let subagent_id: AgentRunId = id("subagent-run");
    let advisor_id: AgentRunId = id("advisor-run");

    let subagent = format_record_dir(&index(
        &request_id,
        &task_id,
        &subagent_id,
        TaskAgentRunKind::Parented {
            parent_agent_run_id: parent_agent_run_id.clone(),
            kind: ParentedAgentRunKind::Subagent,
        },
        None,
    ));
    assert_eq!(
        subagent.as_str(),
        "requests/req-1/subagents/subagent-run-subagent-run"
    );

    let advisor = format_record_dir(&index(
        &request_id,
        &task_id,
        &advisor_id,
        TaskAgentRunKind::Parented {
            parent_agent_run_id,
            kind: ParentedAgentRunKind::Advisor,
        },
        Some(AgentRunRecordDir::new(
            "requests/req-1/root-task-root/agent-run-parent-run",
        )),
    ));
    assert_eq!(
        advisor.as_str(),
        "requests/req-1/root-task-root/agent-run-parent-run/advisors/advisor-run-advisor-run"
    );
}

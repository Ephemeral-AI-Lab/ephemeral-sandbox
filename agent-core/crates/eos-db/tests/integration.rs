//! Cross-store integration tests against a real temp `SQLite` file.
#![allow(clippy::expect_used)]

use eos_db::{Database, DatabaseConfig, DatabaseUrl};
use eos_types::{
    AdvisorVerdict, AgentName, AttemptBudget, AttemptClosure, DeferredGoal, ExecutionNode,
    IterationCreationReason, IterationStatus, JsonObject, ParentAgentRunAnchor,
    ParentedAgentRunKind, ParentedOutcome, PlanId, RequestId, RequestStatus, Task, TaskOutcome,
    TaskRole, TaskStatus, ToolUseId, UtcDateTime, WorkItemId, WorkItemSpec, WorkflowCoordinates,
    WorkflowStatus, WorkflowTaskRole,
};
use serde_json::json;
use sqlx::Row;

async fn open_temp() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.db");
    let mut cfg = DatabaseConfig::default();
    cfg.url = DatabaseUrl::parse(format!("sqlite://{}", path.display())).expect("url");
    let db = Database::open(&cfg).await.expect("open");
    (dir, db)
}

async fn table_column_names(db: &Database, table: &str) -> Vec<String> {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(db.pool())
        .await
        .expect("table_info")
        .into_iter()
        .map(|row| row.get("name"))
        .collect()
}

fn rid(s: &str) -> RequestId {
    s.parse().expect("request id")
}

fn tid(s: &str) -> eos_types::TaskId {
    s.parse().expect("task id")
}

fn arid(s: &str) -> eos_types::AgentRunId {
    s.parse().expect("agent run id")
}

fn wid(s: &str) -> eos_types::WorkflowId {
    s.parse().expect("workflow id")
}

fn iid(s: &str) -> eos_types::IterationId {
    s.parse().expect("iteration id")
}

fn aid(s: &str) -> eos_types::AttemptId {
    s.parse().expect("attempt id")
}

fn tool_use_id(s: &str) -> ToolUseId {
    s.parse().expect("tool use id")
}

fn agent_name(s: &str) -> AgentName {
    AgentName::new(s).expect("agent name")
}

fn work_item_id(s: &str) -> WorkItemId {
    WorkItemId::new(s).expect("work item id")
}

fn plan_id(s: &str) -> PlanId {
    PlanId::new(s).expect("plan id")
}

fn json_obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn task(id: &str, request_id: &RequestId, role: TaskRole, instruction: &str) -> Task {
    Task {
        id: tid(id),
        request_id: request_id.clone(),
        role,
        instruction: instruction.to_owned(),
        status: TaskStatus::Running,
        agent_name: Some(role.as_str().to_owned()),
        task_outcome: None,
    }
}

#[tokio::test]
async fn schema_uses_final_workflow_worker_contract_columns() {
    let (_dir, db) = open_temp().await;

    assert_eq!(
        table_column_names(&db, "tasks").await,
        [
            "id",
            "request_id",
            "role",
            "instruction",
            "status",
            "agent_name",
            "task_outcome",
            "created_at",
            "updated_at",
        ]
    );
    assert_eq!(
        table_column_names(&db, "workflows").await,
        [
            "id",
            "request_id",
            "parent_task_id",
            "parent_agent_run_id",
            "tool_use_id",
            "workflow_goal",
            "status",
            "iteration_ids",
            "created_at",
            "updated_at",
            "closed_at",
        ]
    );
    assert_eq!(
        table_column_names(&db, "iterations").await,
        [
            "id",
            "workflow_id",
            "sequence_no",
            "creation_reason",
            "workflow_goal",
            "iteration_goal",
            "attempt_budget",
            "status",
            "attempt_ids",
            "created_at",
            "updated_at",
            "closed_at",
        ]
    );
    assert_eq!(
        table_column_names(&db, "attempts").await,
        [
            "id",
            "iteration_id",
            "workflow_id",
            "attempt_sequence_no",
            "stage",
            "status",
            "plan_id",
            "execution_tree",
            "fail_reason",
            "created_at",
            "updated_at",
            "closed_at",
        ]
    );
    assert_eq!(
        table_column_names(&db, "task_runs").await,
        [
            "task_id",
            "agent_run_id",
            "request_id",
            "role",
            "status",
            "agent_name",
            "terminal_payload",
            "task_outcome",
            "token_count",
            "error",
            "created_at",
            "updated_at",
            "finished_at",
        ]
    );
    assert_eq!(
        table_column_names(&db, "parented_runs").await,
        [
            "task_id",
            "agent_run_id",
            "request_id",
            "status",
            "parent_agent_run_id",
            "parent_task_id",
            "kind",
            "tool_use_id",
            "agent_name",
            "terminal_payload",
            "parented_outcome",
            "token_count",
            "error",
            "created_at",
            "updated_at",
            "finished_at",
        ]
    );
}

#[tokio::test]
async fn task_workflow_iteration_and_attempt_roundtrip_final_fields() {
    let (_dir, db) = open_temp().await;
    let request_id = rid("req-1");
    db.requests()
        .create_request(&request_id, "/work", None, "build")
        .await
        .expect("create request");

    let worker = task("task-worker", &request_id, TaskRole::Worker, "do work");
    db.tasks().insert_task(&worker).await.expect("insert task");
    let outcome = TaskOutcome::Worker {
        is_pass: true,
        outcome: "done".to_owned(),
    };
    let done = db
        .tasks()
        .set_task_status_if_current(
            &worker.id,
            TaskStatus::Running,
            TaskStatus::Done,
            Some(&outcome),
        )
        .await
        .expect("finish task")
        .expect("task");
    assert_eq!(done.task_outcome, Some(outcome));

    let workflow = db
        .workflows()
        .insert(
            &request_id,
            &worker.id,
            &arid("run-parent"),
            Some(&tool_use_id("toolu-workflow")),
            "workflow goal",
        )
        .await
        .expect("insert workflow");
    assert_eq!(workflow.workflow_goal, "workflow goal");
    assert_eq!(workflow.parent_agent_run_id, arid("run-parent"));

    let iteration = db
        .iterations()
        .insert(
            &workflow.id,
            1,
            IterationCreationReason::Initial,
            "workflow goal",
            "iteration goal",
            AttemptBudget::try_from_u32(2).expect("budget"),
        )
        .await
        .expect("insert iteration");
    assert_eq!(iteration.workflow_goal, "workflow goal");
    assert_eq!(iteration.iteration_goal, "iteration goal");
    db.workflows()
        .append_iteration_id(&workflow.id, &iteration.id)
        .await
        .expect("append iteration");

    let attempt = db
        .attempts()
        .insert(&iteration.id, &workflow.id, 1)
        .await
        .expect("insert attempt");
    assert_eq!(attempt.plan_id, attempt.execution_tree.plan_id);
    assert!(attempt.execution_tree.planner_task_id.is_none());
    assert!(attempt.execution_tree.nodes.is_empty());

    let planner_task_id = tid("task-planner");
    let planned = db
        .attempts()
        .bind_planner_task(&attempt.id, &planner_task_id)
        .await
        .expect("bind planner");
    assert_eq!(
        planned.execution_tree.planner_task_id,
        Some(planner_task_id)
    );

    let nodes = vec![
        ExecutionNode {
            work_item_id: work_item_id("w1"),
            needs: Vec::new(),
            task_id: None,
        },
        ExecutionNode {
            work_item_id: work_item_id("w2"),
            needs: vec![work_item_id("w1")],
            task_id: None,
        },
    ];
    let running = db
        .attempts()
        .record_plan_nodes(&attempt.id, &nodes)
        .await
        .expect("record nodes");
    assert_eq!(running.execution_tree.nodes, nodes);
    assert_eq!(running.stage(), eos_types::AttemptStage::Run);

    let bound = db
        .attempts()
        .bind_worker_task(&attempt.id, &work_item_id("w1"), &worker.id)
        .await
        .expect("bind worker");
    assert_eq!(
        bound
            .execution_tree
            .node(&work_item_id("w1"))
            .and_then(|node| node.task_id.as_ref()),
        Some(&worker.id)
    );

    let closed = db
        .attempts()
        .close(
            &attempt.id,
            AttemptClosure::Passed {
                closed_at: UtcDateTime::now(),
            },
        )
        .await
        .expect("close attempt");
    assert_eq!(closed.status(), eos_types::AttemptStatus::Passed);
    assert!(closed.closed_at().is_some());

    let closed_iteration = db
        .iterations()
        .set_status(
            &iteration.id,
            IterationStatus::Succeeded,
            Some(UtcDateTime::now()),
        )
        .await
        .expect("close iteration");
    assert_eq!(closed_iteration.status, IterationStatus::Succeeded);

    let closed_workflow = db
        .workflows()
        .set_status(
            &workflow.id,
            WorkflowStatus::Succeeded,
            Some(UtcDateTime::now()),
        )
        .await
        .expect("close workflow");
    assert_eq!(closed_workflow.status, WorkflowStatus::Succeeded);
}

#[tokio::test]
async fn task_agent_run_typed_outcomes_roundtrip() {
    let (_dir, db) = open_temp().await;
    let request_id = rid("req-runs");
    db.requests()
        .create_request(&request_id, "/work", None, "run")
        .await
        .expect("create request");

    let root_run = db
        .task_agent_runs()
        .create_root_task_agent_run(&request_id, &arid("run-root"), &agent_name("root"))
        .await
        .expect("root run");
    let root_payload = json_obj(&[
        ("kind", json!("root")),
        ("is_pass", json!(true)),
        ("outcome", json!("ok")),
    ]);
    let root_outcome = TaskOutcome::Root {
        is_pass: true,
        outcome: "ok".to_owned(),
    };
    let finished_root = db
        .task_agent_runs()
        .finish_task_run(
            &root_run.agent_run_id,
            TaskStatus::Done,
            Some(&root_payload),
            Some(&root_outcome),
            9,
            None,
        )
        .await
        .expect("finish root")
        .expect("root row");
    assert_eq!(finished_root.task_outcome, Some(root_outcome));
    assert_eq!(finished_root.terminal_payload, Some(root_payload));

    let coords = WorkflowCoordinates {
        workflow_id: wid("wf-runs"),
        iteration_id: iid("it-runs"),
        attempt_id: aid("att-runs"),
    };
    let work_item_id = work_item_id("w1");
    let worker_run = db
        .task_agent_runs()
        .create_workflow_task_agent_run(
            &request_id,
            &arid("run-worker"),
            &coords,
            WorkflowTaskRole::Worker,
            &plan_id("plan-1"),
            Some(&work_item_id),
            &agent_name("executor"),
        )
        .await
        .expect("worker run");
    let worker_outcome = TaskOutcome::Worker {
        is_pass: false,
        outcome: "blocked".to_owned(),
    };
    let finished_worker = db
        .task_agent_runs()
        .finish_task_run(
            &worker_run.agent_run_id,
            TaskStatus::Failed,
            None,
            Some(&worker_outcome),
            3,
            Some("model stopped"),
        )
        .await
        .expect("finish worker")
        .expect("worker row");
    assert_eq!(finished_worker.role, TaskRole::Worker);
    assert_eq!(finished_worker.task_outcome, Some(worker_outcome));
    assert_eq!(finished_worker.error.as_deref(), Some("model stopped"));

    let parent = ParentAgentRunAnchor {
        request_id: request_id.clone(),
        parent_task_id: root_run.task_id,
        agent_run_id: arid("run-root"),
    };
    let advisor_run = db
        .task_agent_runs()
        .create_parented_task_agent_run(
            &arid("run-advisor"),
            &parent,
            ParentedAgentRunKind::Advisor,
            Some(&tool_use_id("toolu-advisor")),
            &agent_name("advisor"),
        )
        .await
        .expect("advisor run");
    let parented_outcome = ParentedOutcome::Advisor {
        verdict: AdvisorVerdict::Approve,
        outcome: "approved".to_owned(),
    };
    let finished_parented = db
        .task_agent_runs()
        .finish_parented_run(
            &advisor_run.agent_run_id,
            TaskStatus::Done,
            None,
            Some(&parented_outcome),
            2,
            None,
        )
        .await
        .expect("finish advisor")
        .expect("parented row");
    assert_eq!(finished_parented.parented_outcome, Some(parented_outcome));
}

#[tokio::test]
async fn planner_outcome_json_roundtrips_through_task_store() {
    let (_dir, db) = open_temp().await;
    let request_id = rid("req-plan");
    db.requests()
        .create_request(&request_id, "/work", None, "plan")
        .await
        .expect("create request");
    let planner = task("task-planner", &request_id, TaskRole::Planner, "plan");
    db.tasks()
        .insert_task(&planner)
        .await
        .expect("insert planner");

    let outcome = TaskOutcome::Planner {
        plan_spec: "plan spec".to_owned(),
        work_items: vec![WorkItemSpec {
            id: work_item_id("w1"),
            agent_name: agent_name("executor"),
            work_spec: "do one thing".to_owned(),
            needs: Vec::new(),
        }],
        deferred_goal_for_next_iteration: Some(
            DeferredGoal::new("next iteration").expect("deferred goal"),
        ),
    };
    let done = db
        .tasks()
        .set_task_status_if_current(
            &planner.id,
            TaskStatus::Running,
            TaskStatus::Done,
            Some(&outcome),
        )
        .await
        .expect("finish planner")
        .expect("planner");
    assert_eq!(done.task_outcome, Some(outcome.clone()));
    let reloaded = db
        .tasks()
        .get(&planner.id)
        .await
        .expect("reload planner")
        .expect("planner present");
    assert_eq!(reloaded.task_outcome, Some(outcome));
}

#[tokio::test]
async fn request_finish_remains_terminal_noop() {
    let (_dir, db) = open_temp().await;
    let request_id = rid("req-terminal");
    db.requests()
        .create_request(&request_id, "/work", None, "done")
        .await
        .expect("create request");
    db.requests()
        .finish_request(&request_id, RequestStatus::Done)
        .await
        .expect("finish")
        .expect("request");
    let again = db
        .requests()
        .finish_request(&request_id, RequestStatus::Failed)
        .await
        .expect("finish again")
        .expect("request");
    assert_eq!(again.status, RequestStatus::Done);
}

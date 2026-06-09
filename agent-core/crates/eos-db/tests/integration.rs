//! Cross-store integration tests against a real temp `SQLite` file (AC-eos-db-01..05, 08).
#![allow(clippy::expect_used)]

use eos_db::{Database, DatabaseConfig, DatabaseUrl};
use eos_types::{
    format_record_dir, AgentName, AttemptBudget, AttemptClosure, AttemptFailReason, AttemptStage,
    AttemptStatus, DeferredGoal, ExecutionRole, ExecutionTaskOutcome, IterationCreationReason,
    IterationStatus, JsonObject, MaterializedPlan, ParentAgentRunAnchor, ParentedAgentRunKind,
    PlanDisposition, PlannerId, RequestId, RequestStatus, Task, TaskAgentRunKind, TaskId, TaskRole,
    TaskStatus, ToolUseId, UtcDateTime, WorkflowCoordinates, WorkflowNodeId, WorkflowStatus,
    WorkflowTaskRole,
};
use sqlx::Row;

async fn open_temp() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.db");
    let mut cfg = DatabaseConfig::default();
    cfg.url = DatabaseUrl::parse(format!("sqlite://{}", path.display())).expect("url");
    let db = Database::open(&cfg).await.expect("open");
    (dir, db)
}

#[derive(Debug, PartialEq, Eq)]
struct ColumnInfo {
    name: String,
    type_name: String,
    default_value: Option<String>,
    primary_key: i64,
}

async fn table_columns(db: &Database, table: &str) -> Vec<ColumnInfo> {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(db.pool())
        .await
        .expect("table_info")
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.get("name"),
            type_name: row.get("type"),
            default_value: row.get("dflt_value"),
            primary_key: row.get("pk"),
        })
        .collect()
}

fn col(name: &str, type_name: &str, default_value: Option<&str>, primary_key: i64) -> ColumnInfo {
    ColumnInfo {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        default_value: default_value.map(ToOwned::to_owned),
        primary_key,
    }
}

fn rid(s: &str) -> RequestId {
    s.parse().expect("request id")
}

fn tid(s: &str) -> TaskId {
    s.parse().expect("task id")
}

fn arid(s: &str) -> eos_types::AgentRunId {
    s.parse().expect("agent run id")
}

fn tool_use_id(s: &str) -> ToolUseId {
    s.parse().expect("tool use id")
}

fn agent_name(s: &str) -> AgentName {
    AgentName::new(s).expect("agent name")
}

fn sample_task(id: &str, request_id: &RequestId, instruction: &str) -> Task {
    Task {
        id: tid(id),
        request_id: request_id.clone(),
        role: TaskRole::Generator,
        instruction: instruction.to_owned(),
        status: TaskStatus::Pending,
        workflow_id: None,
        iteration_id: None,
        attempt_id: None,
        agent_name: Some("coder".to_owned()),
        needs: Vec::new(),
        outcomes: Vec::new(),
        terminal_payload: None,
    }
}

fn json_obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

// AC-eos-db-01: request + task roundtrip, terminal no-op, upsert, CAS.
#[tokio::test]
async fn request_task_roundtrip() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let tasks = db.tasks();
    let id = rid("req-1");

    requests
        .create_request(&id, "/work", None, "build the thing")
        .await
        .expect("create");
    let got = requests.get(&id).await.expect("get").expect("present");
    assert_eq!(got.cwd, "/work");
    assert_eq!(got.request_prompt, "build the thing");
    assert_eq!(got.status, RequestStatus::Running);

    let with_root = requests
        .set_root_task_id(&id, &tid("root-1"))
        .await
        .expect("set root");
    assert_eq!(with_root.root_task_id, Some(tid("root-1")));

    let done = requests
        .finish_request(&id, RequestStatus::Done)
        .await
        .expect("finish")
        .expect("some");
    assert_eq!(done.status, RequestStatus::Done);
    assert!(done.finished_at.is_some());
    // Terminal no-op: a second finish leaves the request unchanged.
    let again = requests
        .finish_request(&id, RequestStatus::Failed)
        .await
        .expect("finish2")
        .expect("some");
    assert_eq!(again.status, RequestStatus::Done);

    // Insert creates a task; duplicate ids are rejected instead of overwriting
    // lifecycle-sensitive fields.
    let t = sample_task("t-1", &id, "first");
    tasks.insert_task(&t).await.expect("insert");
    assert_eq!(
        tasks
            .get(&t.id)
            .await
            .expect("get")
            .expect("present")
            .instruction,
        "first"
    );
    let duplicate = tasks.insert_task(&t).await;
    assert!(duplicate.is_err());

    // CAS: mismatch is a no-op; match flips.
    let miss = tasks
        .set_task_status_if_current(&t.id, TaskStatus::Running, TaskStatus::Done, None, None)
        .await
        .expect("cas");
    assert!(miss.is_none());
    let hit = tasks
        .set_task_status_if_current(&t.id, TaskStatus::Pending, TaskStatus::Running, None, None)
        .await
        .expect("cas")
        .expect("flipped");
    assert_eq!(hit.status, TaskStatus::Running);
}

// AC-eos-db-02: workflow roundtrip + the goal -> workflow_goal naming gap.
#[tokio::test]
async fn workflow_roundtrip_goal_mapping() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let workflows = db.workflows();
    let id = rid("req-2");
    requests
        .create_request(&id, "/w", None, "p")
        .await
        .expect("create");

    let parent = tid("parent-1");
    let wf = workflows
        .insert(&id, &parent, &arid("launch-1"), None, "build the parser")
        .await
        .expect("insert");
    assert_eq!(wf.workflow_goal, "build the parser");
    assert!(wf.is_open());

    // The raw DB column is `goal`, the domain field is `workflow_goal`.
    let raw_goal: String =
        sqlx::query_scalar::<sqlx::Sqlite, String>("SELECT goal FROM workflows WHERE id = ?")
            .bind(wf.id.as_str())
            .fetch_one(db.pool())
            .await
            .expect("raw goal");
    assert_eq!(raw_goal, "build the parser");

    let it_id: eos_types::IterationId = "iter-x".parse().expect("iter id");
    let appended = workflows
        .append_iteration_id(&wf.id, &it_id)
        .await
        .expect("append");
    assert_eq!(appended.iteration_ids, vec![it_id]);

    let now = UtcDateTime::now();
    let closed = workflows
        .set_status(&wf.id, WorkflowStatus::Succeeded, Some(now), Some("[]"))
        .await
        .expect("set status");
    assert_eq!(closed.status, WorkflowStatus::Succeeded);
    assert_eq!(closed.outcomes.as_deref(), Some("[]"));
    assert!(closed.closed_at.is_some());

    let listed = workflows.list_for_parent_task(&parent).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, wf.id);
}

// AC-eos-db-03: iteration roundtrip, close_succeeded, deferred_goal naming, unique.
#[tokio::test]
async fn iteration_roundtrip() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let workflows = db.workflows();
    let iterations = db.iterations();
    let id = rid("req-3");
    requests
        .create_request(&id, "/w", None, "p")
        .await
        .expect("create");
    let wf = workflows
        .insert(&id, &tid("p3"), &arid("launch-p3"), None, "goal")
        .await
        .expect("wf");

    let it = iterations
        .insert(
            &wf.id,
            0,
            IterationCreationReason::Initial,
            "iterate well",
            AttemptBudget::try_from_u32(3).expect("budget"),
        )
        .await
        .expect("insert");
    assert_eq!(it.iteration_goal, "iterate well");
    assert_eq!(it.status, IterationStatus::Open);
    assert_eq!(it.attempt_budget.get(), 3);

    let next_time = DeferredGoal::new("next time").expect("goal");
    let deferred = iterations
        .set_deferred_goal_for_next_iteration(&it.id, Some(&next_time))
        .await
        .expect("set deferred");
    assert_eq!(
        deferred
            .deferred_goal_for_next_iteration
            .as_ref()
            .map(DeferredGoal::as_str),
        Some("next time")
    );
    // Raw column is `deferred_goal`.
    let raw: Option<String> = sqlx::query_scalar::<sqlx::Sqlite, Option<String>>(
        "SELECT deferred_goal FROM iterations WHERE id = ?",
    )
    .bind(it.id.as_str())
    .fetch_one(db.pool())
    .await
    .expect("raw deferred");
    assert_eq!(raw.as_deref(), Some("next time"));

    let now = UtcDateTime::now();
    let closed = iterations
        .close_succeeded(&it.id, "[{\"x\":1}]", Some(now))
        .await
        .expect("close");
    assert_eq!(closed.status, IterationStatus::Succeeded);
    assert_eq!(closed.outcomes.as_deref(), Some("[{\"x\":1}]"));
    assert!(closed.closed_at.is_some());

    // Unique (workflow_id, sequence_no): a duplicate insert errors.
    assert!(iterations
        .insert(
            &wf.id,
            0,
            IterationCreationReason::Initial,
            "dup",
            AttemptBudget::try_from_u32(1).expect("budget"),
        )
        .await
        .is_err());

    let listed = iterations.list_for_workflow(&wf.id).await.expect("list");
    assert_eq!(listed.len(), 1);
}

// AC-eos-db-04: attempt roundtrip, outcome parse, unique.
#[tokio::test]
async fn attempt_roundtrip() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let workflows = db.workflows();
    let iterations = db.iterations();
    let attempts = db.attempts();
    let id = rid("req-4");
    requests
        .create_request(&id, "/w", None, "p")
        .await
        .expect("create");
    let wf = workflows
        .insert(&id, &tid("p4"), &arid("launch-p4"), None, "goal")
        .await
        .expect("wf");
    let it = iterations
        .insert(
            &wf.id,
            0,
            IterationCreationReason::Initial,
            "g",
            AttemptBudget::try_from_u32(3).expect("budget"),
        )
        .await
        .expect("it");

    let att = attempts.insert(&it.id, &wf.id, 0).await.expect("insert");
    assert_eq!(att.stage(), AttemptStage::Plan);
    assert_eq!(att.status(), AttemptStatus::Running);
    assert!(att.outcomes().is_empty());

    attempts
        .record_planner_task(&att.id, &tid("planner-1"))
        .await
        .expect("planner");
    let with_red = attempts
        .record_plan(
            &att.id,
            &MaterializedPlan {
                planner_task_id: tid("planner-1"),
                disposition: PlanDisposition::Complete,
                generator_task_ids: vec![tid("g1"), tid("g2")],
                reducer_task_ids: vec![tid("r1")],
            },
        )
        .await
        .expect("plan");
    assert_eq!(with_red.generator_task_ids(), &[tid("g1"), tid("g2")]);
    assert_eq!(with_red.reducer_task_ids(), &[tid("r1")]);

    let outcomes = vec![ExecutionTaskOutcome {
        status: eos_types::TaskOutcomeStatus::Failed,
        role: ExecutionRole::Generator,
        task_id: tid("g1"),
        outcome: "boom".to_owned(),
    }];
    let closed = attempts
        .close(
            &att.id,
            AttemptClosure::Failed {
                reason: AttemptFailReason::TaskFailed,
                outcomes: outcomes.clone(),
                closed_at: UtcDateTime::now(),
            },
        )
        .await
        .expect("close");
    assert_eq!(closed.stage(), AttemptStage::Closed);
    assert_eq!(closed.status(), AttemptStatus::Failed);
    assert_eq!(closed.fail_reason(), Some(AttemptFailReason::TaskFailed));
    assert_eq!(closed.outcomes(), outcomes); // round-trips through normalization

    // Unique (iteration_id, attempt_sequence_no).
    assert!(attempts.insert(&it.id, &wf.id, 0).await.is_err());

    let listed = attempts.list_for_iteration(&it.id).await.expect("list");
    assert_eq!(listed.len(), 1);
}

// AC-eos-db-04b: agent-run roundtrip + unique task_id; null-preserving columns.
#[tokio::test]
async fn agent_run_roundtrip() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let tasks = db.tasks();
    let agent_runs = db.agent_runs();
    let id = rid("req-5");
    requests
        .create_request(&id, "/w", None, "p")
        .await
        .expect("create");
    tasks
        .insert_task(&sample_task("t-5", &id, "do"))
        .await
        .expect("task");

    let run_id: eos_types::AgentRunId = "run-1".parse().expect("run id");
    let created = agent_runs
        .create_run(&run_id, Some(&tid("t-5")), "coder")
        .await
        .expect("create run");
    assert_eq!(created.agent_name, "coder");
    assert!(created.terminal_payload.is_none());
    assert_eq!(created.token_count, 0);

    let payload = json_obj(&[("ok", serde_json::json!(true))]);
    let finished = agent_runs
        .finish_run(&run_id, Some(&payload), 42, None)
        .await
        .expect("finish")
        .expect("some");
    assert_eq!(finished.terminal_payload, Some(payload));
    assert_eq!(finished.token_count, 42);
    assert!(finished.finished_at.is_some());

    // Unique task_id: a second run for the same task errors.
    assert!(agent_runs
        .create_run(
            &"run-2".parse::<eos_types::AgentRunId>().expect("id"),
            Some(&tid("t-5")),
            "coder"
        )
        .await
        .is_err());
}

#[tokio::test]
async fn task_agent_run_lineage_materializes_record_indexes() {
    let (_dir, db) = open_temp().await;
    let request_id = rid("req-lineage");
    db.requests()
        .create_request(&request_id, "/w", None, "p")
        .await
        .expect("request");

    let root_task = eos_types::root_task_id(&request_id);
    let root_run = arid("run-root-lineage");
    let created_root = db
        .task_agent_runs()
        .create_root_task_agent_run(&request_id, &root_run, &agent_name("root"))
        .await
        .expect("root");
    assert_eq!(created_root.task_id, root_task);
    assert_eq!(
        created_root.record_target.record_dir.as_str(),
        "requests/req-lineage/root-task-root-req-lineage/agent-run-run-root-lineage"
    );
    assert_eq!(
        db.requests()
            .get(&request_id)
            .await
            .expect("get request")
            .expect("request")
            .root_task_id,
        Some(root_task.clone())
    );
    let root_payload = json_obj(&[("result", serde_json::json!("root done"))]);
    let finished_root = db
        .task_agent_runs()
        .finish_task_run(&root_run, TaskStatus::Done, Some(&root_payload), 7, None)
        .await
        .expect("finish root")
        .expect("root run");
    assert_eq!(finished_root.status, TaskStatus::Done);
    assert_eq!(finished_root.terminal_payload, Some(root_payload));
    assert_eq!(finished_root.token_count, 7);

    let workflow_tool_use_id = tool_use_id("tool-workflow");
    let workflow = db
        .workflows()
        .insert(
            &request_id,
            &root_task,
            &root_run,
            Some(&workflow_tool_use_id),
            "delegated goal",
        )
        .await
        .expect("workflow");
    assert_eq!(workflow.launched_by_agent_run_id, root_run.clone());
    assert_eq!(workflow.tool_use_id, Some(workflow_tool_use_id));
    assert_eq!(
        db.workflows()
            .list_for_launching_agent_run(&root_run)
            .await
            .expect("by launch")
            .len(),
        1
    );

    let workflow_coords = WorkflowCoordinates {
        workflow_id: workflow.id.clone(),
        iteration_id: "iter-lineage".parse().expect("iteration id"),
        attempt_id: "attempt-lineage".parse().expect("attempt id"),
    };
    let planner_run = arid("run-planner-lineage");
    db.task_agent_runs()
        .create_workflow_task_agent_run(
            &request_id,
            &planner_run,
            &workflow_coords,
            &WorkflowNodeId::Planner {
                planner_id: PlannerId::new("planner").expect("planner id"),
            },
            &agent_name("planner"),
        )
        .await
        .expect("planner");
    let planner_index = db
        .task_agent_runs()
        .record_index_for_agent_run(&planner_run)
        .await
        .expect("planner index")
        .expect("planner index");
    assert_eq!(
        planner_index.kind,
        TaskAgentRunKind::Workflow {
            workflow: workflow_coords,
            role: WorkflowTaskRole::Planner,
        }
    );

    let subagent_run = arid("run-sub-lineage");
    let advisor_run = arid("run-advisor-lineage");
    let parent = ParentAgentRunAnchor {
        request_id: request_id.clone(),
        parent_task_id: root_task.clone(),
        agent_run_id: root_run.clone(),
    };
    let created_subagent = db
        .task_agent_runs()
        .create_parented_task_agent_run(
            &subagent_run,
            &parent,
            ParentedAgentRunKind::Subagent,
            Some(&tool_use_id("tool-sub")),
            &agent_name("worker"),
        )
        .await
        .expect("subagent");
    assert_eq!(
        created_subagent.record_target.record_dir.as_str(),
        "requests/req-lineage/root-task-root-req-lineage/agent-run-run-root-lineage/subagents/subagent-run-run-sub-lineage"
    );
    let created_advisor = db
        .task_agent_runs()
        .create_parented_task_agent_run(
            &advisor_run,
            &parent,
            ParentedAgentRunKind::Advisor,
            Some(&tool_use_id("tool-advisor")),
            &agent_name("advisor"),
        )
        .await
        .expect("advisor");
    assert_eq!(
        created_advisor.record_target.record_dir.as_str(),
        "requests/req-lineage/root-task-root-req-lineage/agent-run-run-root-lineage/advisors/advisor-run-run-advisor-lineage"
    );
    let advisor_payload = json_obj(&[("feedback", serde_json::json!("ship"))]);
    let finished_advisor = db
        .task_agent_runs()
        .finish_parented_run(
            &advisor_run,
            TaskStatus::Done,
            Some(&advisor_payload),
            3,
            None,
        )
        .await
        .expect("finish advisor")
        .expect("advisor run");
    assert_eq!(finished_advisor.status, TaskStatus::Done);
    assert_eq!(finished_advisor.terminal_payload, Some(advisor_payload));
    let subagent_index = db
        .task_agent_runs()
        .record_index_for_agent_run(&subagent_run)
        .await
        .expect("subagent index")
        .expect("subagent index");
    assert_eq!(
        format_record_dir(&subagent_index).as_str(),
        created_subagent.record_target.record_dir.as_str()
    );
    assert!(matches!(
        subagent_index.kind,
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Subagent,
            ..
        }
    ));

    let index = db
        .task_agent_runs()
        .task_execution_index(&root_task)
        .await
        .expect("flat index")
        .expect("flat index");
    assert_eq!(index.agent_run_id, root_run);
    assert_eq!(index.workflow_ids, vec![workflow.id]);
    assert_eq!(index.subagent_ids, vec![subagent_run]);
    assert_eq!(index.advisor_ids, vec![advisor_run]);
}

// AC-eos-db-05: migrations build the full schema with final column names + FKs on.
#[tokio::test]
async fn migrations_create_schema() {
    let (_dir, db) = open_temp().await;
    for table in [
        "requests",
        "tasks",
        "workflows",
        "task_runs",
        "parented_runs",
        "iterations",
        "attempts",
        "agent_runs",
        "model_registrations",
    ] {
        let found: Option<String> = sqlx::query_scalar::<sqlx::Sqlite, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_optional(db.pool())
        .await
        .expect("query master");
        assert_eq!(found.as_deref(), Some(table), "missing table {table}");
    }

    // Final (renamed) column names exist.
    for sql in [
        "SELECT instruction FROM tasks LIMIT 0",
        "SELECT request_id FROM tasks LIMIT 0",
        "SELECT launched_by_agent_run_id FROM workflows LIMIT 0",
        "SELECT agent_run_id FROM task_runs LIMIT 0",
        "SELECT parent_agent_run_id FROM parented_runs LIMIT 0",
        "SELECT outcomes FROM iterations LIMIT 0",
    ] {
        sqlx::query(sql).fetch_optional(db.pool()).await.expect(sql);
    }

    // Foreign keys are enforced.
    let fk: i64 = sqlx::query_scalar::<sqlx::Sqlite, i64>("PRAGMA foreign_keys")
        .fetch_one(db.pool())
        .await
        .expect("pragma fk");
    assert_eq!(fk, 1);

    assert_eq!(
        table_columns(&db, "requests").await,
        vec![
            col("id", "TEXT", None, 1),
            col("cwd", "TEXT", None, 0),
            col("sandbox_id", "TEXT", None, 0),
            col("request_prompt", "TEXT", None, 0),
            col("root_task_id", "TEXT", None, 0),
            col("status", "TEXT", Some("'running'"), 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
            col("finished_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "tasks").await,
        vec![
            col("id", "TEXT", None, 1),
            col("request_id", "TEXT", None, 0),
            col("role", "TEXT", None, 0),
            col("instruction", "TEXT", None, 0),
            col("status", "TEXT", None, 0),
            col("workflow_id", "TEXT", None, 0),
            col("iteration_id", "TEXT", None, 0),
            col("attempt_id", "TEXT", None, 0),
            col("agent_name", "TEXT", None, 0),
            col("needs", "TEXT", Some("'[]'"), 0),
            col("outcomes", "TEXT", Some("'[]'"), 0),
            col("terminal_tool_result", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "workflows").await,
        vec![
            col("id", "TEXT", None, 1),
            col("request_id", "TEXT", None, 0),
            col("parent_task_id", "TEXT", None, 0),
            col("launched_by_agent_run_id", "TEXT", None, 0),
            col("tool_use_id", "TEXT", None, 0),
            col("goal", "TEXT", None, 0),
            col("status", "TEXT", None, 0),
            col("iteration_ids", "TEXT", Some("'[]'"), 0),
            col("outcomes", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
            col("closed_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "task_runs").await,
        vec![
            col("task_id", "TEXT", None, 1),
            col("agent_run_id", "TEXT", None, 0),
            col("request_id", "TEXT", None, 0),
            col("role", "TEXT", None, 0),
            col("status", "TEXT", None, 0),
            col("workflow_id", "TEXT", None, 0),
            col("iteration_id", "TEXT", None, 0),
            col("attempt_id", "TEXT", None, 0),
            col("agent_name", "TEXT", None, 0),
            col("terminal_payload", "TEXT", None, 0),
            col("token_count", "INTEGER", Some("0"), 0),
            col("error", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
            col("finished_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "parented_runs").await,
        vec![
            col("task_id", "TEXT", None, 1),
            col("agent_run_id", "TEXT", None, 0),
            col("request_id", "TEXT", None, 0),
            col("status", "TEXT", None, 0),
            col("parent_agent_run_id", "TEXT", None, 0),
            col("parent_task_id", "TEXT", None, 0),
            col("kind", "TEXT", None, 0),
            col("tool_use_id", "TEXT", None, 0),
            col("agent_name", "TEXT", None, 0),
            col("terminal_payload", "TEXT", None, 0),
            col("token_count", "INTEGER", Some("0"), 0),
            col("error", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
            col("finished_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "iterations").await,
        vec![
            col("id", "TEXT", None, 1),
            col("workflow_id", "TEXT", None, 0),
            col("sequence_no", "INTEGER", None, 0),
            col("creation_reason", "TEXT", None, 0),
            col("goal", "TEXT", None, 0),
            col("attempt_budget", "INTEGER", None, 0),
            col("status", "TEXT", None, 0),
            col("attempt_ids", "TEXT", Some("'[]'"), 0),
            col("deferred_goal", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
            col("closed_at", "TEXT", None, 0),
            col("outcomes", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "attempts").await,
        vec![
            col("id", "TEXT", None, 1),
            col("iteration_id", "TEXT", None, 0),
            col("workflow_id", "TEXT", None, 0),
            col("attempt_sequence_no", "INTEGER", None, 0),
            col("stage", "TEXT", None, 0),
            col("status", "TEXT", None, 0),
            col("planner_task_id", "TEXT", None, 0),
            col("generator_task_ids", "TEXT", Some("'[]'"), 0),
            col("reducer_task_ids", "TEXT", Some("'[]'"), 0),
            col("outcomes", "TEXT", Some("'[]'"), 0),
            col("deferred_goal", "TEXT", None, 0),
            col("fail_reason", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
            col("closed_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "agent_runs").await,
        vec![
            col("id", "TEXT", None, 1),
            col("task_id", "TEXT", None, 0),
            col("agent_name", "TEXT", None, 0),
            col("terminal_payload", "TEXT", None, 0),
            col("token_count", "INTEGER", Some("0"), 0),
            col("error", "TEXT", None, 0),
            col("created_at", "TEXT", None, 0),
            col("finished_at", "TEXT", None, 0),
        ]
    );
    assert_eq!(
        table_columns(&db, "model_registrations").await,
        vec![
            col("id", "INTEGER", None, 1),
            col("key", "TEXT", None, 0),
            col("label", "TEXT", None, 0),
            col("class_path", "TEXT", None, 0),
            col("kwargs_json", "TEXT", Some("'{}'"), 0),
            col("is_active", "INTEGER", Some("0"), 0),
            col("created_at", "TEXT", None, 0),
            col("updated_at", "TEXT", None, 0),
        ]
    );
}

// AC-eos-db-08: composition root yields working stores; deleting a request
// cascades to its tasks and workflows.
#[tokio::test]
async fn composition_root_and_cascade() {
    let (_dir, db) = open_temp().await;
    let id = rid("req-8");
    db.requests()
        .create_request(&id, "/w", None, "p")
        .await
        .expect("create");
    db.tasks()
        .insert_task(&sample_task("t-8", &id, "do"))
        .await
        .expect("task");
    let wf = db
        .workflows()
        .insert(&id, &tid("p8"), &arid("launch-p8"), None, "goal")
        .await
        .expect("wf");

    // Sanity: rows are present before the cascade.
    assert!(db.tasks().get(&tid("t-8")).await.expect("get").is_some());
    assert!(db.workflows().get(&wf.id).await.expect("get").is_some());

    sqlx::query("DELETE FROM requests WHERE id = ?")
        .bind(id.as_str())
        .execute(db.pool())
        .await
        .expect("delete request");

    // FK ON DELETE CASCADE removed the child rows.
    assert!(db.tasks().get(&tid("t-8")).await.expect("get").is_none());
    assert!(db.workflows().get(&wf.id).await.expect("get").is_none());
}

// Read-side store APIs the backend composition root consumes through
// agent-core request reads: request listing with status filter +
// pagination (total ignores the window), the per-request task tree, and the
// latest run for a task.
#[tokio::test]
async fn read_side_list_apis() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let tasks = db.tasks();
    let agent_runs = db.agent_runs();

    // Three requests: one Done, one Failed, one left Running.
    for (name, finish) in [
        ("req-a", Some(RequestStatus::Done)),
        ("req-b", Some(RequestStatus::Failed)),
        ("req-c", None),
    ] {
        let id = rid(name);
        requests
            .create_request(&id, "/w", None, name)
            .await
            .expect("create");
        if let Some(status) = finish {
            requests.finish_request(&id, status).await.expect("finish");
        }
    }

    // Unfiltered list returns every request.
    let all = requests.list().await.expect("list all");
    assert_eq!(all.len(), 3);
    assert!(all.iter().any(|request| request.request_prompt == "req-a"));
    assert!(all.iter().any(|request| request.request_prompt == "req-b"));
    assert!(all.iter().any(|request| request.request_prompt == "req-c"));

    // list_for_request returns only the owning request's tasks.
    let owner = rid("req-a");
    let other = rid("req-b");
    tasks
        .insert_task(&sample_task("t-a1", &owner, "one"))
        .await
        .expect("t-a1");
    tasks
        .insert_task(&sample_task("t-a2", &owner, "two"))
        .await
        .expect("t-a2");
    tasks
        .insert_task(&sample_task("t-b1", &other, "x"))
        .await
        .expect("t-b1");
    let owner_tasks = tasks.list_for_request(&owner).await.expect("list tasks");
    assert_eq!(owner_tasks.len(), 2);
    assert!(owner_tasks.iter().all(|task| task.request_id == owner));

    // get_for_task returns the bound run, and None when a task has no run.
    let run_id: eos_types::AgentRunId = "run-a1".parse().expect("run id");
    agent_runs
        .create_run(&run_id, Some(&tid("t-a1")), "coder")
        .await
        .expect("run");
    let got = agent_runs
        .get_for_task(&tid("t-a1"))
        .await
        .expect("get_for_task")
        .expect("some");
    assert_eq!(got.id, run_id);
    assert!(agent_runs
        .get_for_task(&tid("t-a2"))
        .await
        .expect("none")
        .is_none());
}

// Store mutators on a missing row: the contract distinguishes a "row absent"
// NotFound error from the conditional-update no-op (`Ok(None)`). Every prior
// assertion ran against rows that exist, so these arms were unguarded.
#[tokio::test]
async fn store_mutators_distinguish_missing_and_mismatch() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let tasks = db.tasks();
    let workflows = db.workflows();
    let iterations = db.iterations();
    let attempts = db.attempts();
    let agent_runs = db.agent_runs();

    // finish_request on a nonexistent request is a benign Ok(None), not an error.
    assert!(requests
        .finish_request(&rid("ghost-req"), RequestStatus::Done)
        .await
        .expect("finish missing request is Ok(None)")
        .is_none());
    // set_root_task_id requires the request to exist.
    let err = requests
        .set_root_task_id(&rid("ghost-req"), &tid("root"))
        .await
        .expect_err("missing request is rejected");
    assert!(err.to_string().contains("not found in requests"), "{err}");

    // The set_task_status_if_current missing-vs-mismatch split: a wrong expected
    // status on an existing row is Ok(None) (a no-op); an absent row is NotFound.
    let req = rid("req-nf");
    requests
        .create_request(&req, "/w", None, "p")
        .await
        .expect("request");
    tasks
        .insert_task(&sample_task("t-nf", &req, "do"))
        .await
        .expect("task");
    assert!(
        tasks
            .set_task_status_if_current(
                &tid("t-nf"),
                TaskStatus::Running, // wrong expected (row is Pending)
                TaskStatus::Done,
                None,
                None,
            )
            .await
            .expect("status mismatch is Ok(None)")
            .is_none(),
        "an existing row with a mismatched expected status is a no-op"
    );
    let err = tasks
        .set_task_status_if_current(
            &tid("ghost-task"),
            TaskStatus::Pending,
            TaskStatus::Running,
            None,
            None,
        )
        .await
        .expect_err("missing task is rejected");
    assert!(err.to_string().contains("not found in tasks"), "{err}");

    // Workflow / iteration / attempt mutators on an absent row are NotFound.
    let wf_missing: eos_types::WorkflowId = "ghost-wf".parse().expect("wf id");
    assert!(workflows
        .set_status(&wf_missing, WorkflowStatus::Succeeded, None, None)
        .await
        .expect_err("missing workflow")
        .to_string()
        .contains("not found in workflows"));
    assert!(workflows
        .append_iteration_id(
            &wf_missing,
            &"i".parse::<eos_types::IterationId>().expect("it id")
        )
        .await
        .is_err());

    let it_missing: eos_types::IterationId = "ghost-it".parse().expect("it id");
    assert!(iterations
        .close_succeeded(&it_missing, "[]", None)
        .await
        .expect_err("missing iteration")
        .to_string()
        .contains("not found in iterations"));
    assert!(iterations
        .set_deferred_goal_for_next_iteration(&it_missing, None)
        .await
        .is_err());

    let att_missing: eos_types::AttemptId = "ghost-att".parse().expect("att id");
    assert!(attempts
        .close(
            &att_missing,
            AttemptClosure::Passed {
                outcomes: Vec::new(),
                closed_at: UtcDateTime::now(),
            },
        )
        .await
        .expect_err("missing attempt")
        .to_string()
        .contains("not found in attempts"));
    assert!(attempts
        .record_planner_task(&att_missing, &tid("p"))
        .await
        .is_err());

    // finish_run on a missing run is Ok(None) (mirrors finish_request).
    assert!(agent_runs
        .finish_run(
            &"ghost-run"
                .parse::<eos_types::AgentRunId>()
                .expect("run id"),
            None,
            0,
            None,
        )
        .await
        .expect("finish missing run is Ok(None)")
        .is_none());
}

// A passed attempt round-trips through the store: close as Passed, then a fresh
// SELECT reconstructs the Passed closure via row_to_attempt. Prior tests only
// ever closed Failed, leaving the success-path reconstruction unexercised.
#[tokio::test]
async fn attempt_passed_closure_roundtrips_through_store() {
    let (_dir, db) = open_temp().await;
    let requests = db.requests();
    let workflows = db.workflows();
    let iterations = db.iterations();
    let attempts = db.attempts();
    let id = rid("req-pass");
    requests
        .create_request(&id, "/w", None, "p")
        .await
        .expect("create");
    let wf = workflows
        .insert(&id, &tid("p-pass"), &arid("launch-pass"), None, "goal")
        .await
        .expect("wf");
    let it = iterations
        .insert(
            &wf.id,
            0,
            IterationCreationReason::Initial,
            "g",
            AttemptBudget::try_from_u32(3).expect("budget"),
        )
        .await
        .expect("it");
    let att = attempts.insert(&it.id, &wf.id, 0).await.expect("insert");
    attempts
        .record_planner_task(&att.id, &tid("planner-p"))
        .await
        .expect("planner");
    attempts
        .record_plan(
            &att.id,
            &MaterializedPlan {
                planner_task_id: tid("planner-p"),
                disposition: PlanDisposition::Complete,
                generator_task_ids: vec![tid("g1")],
                reducer_task_ids: vec![tid("r1")],
            },
        )
        .await
        .expect("plan");

    let outcomes = vec![ExecutionTaskOutcome {
        status: eos_types::TaskOutcomeStatus::Success,
        role: ExecutionRole::Reducer,
        task_id: tid("r1"),
        outcome: "shipped".to_owned(),
    }];
    let closed = attempts
        .close(
            &att.id,
            AttemptClosure::Passed {
                outcomes: outcomes.clone(),
                closed_at: UtcDateTime::now(),
            },
        )
        .await
        .expect("close passed");
    assert_eq!(closed.stage(), AttemptStage::Closed);
    assert_eq!(closed.status(), AttemptStatus::Passed);
    assert_eq!(closed.fail_reason(), None); // Passed carries no fail_reason
    assert_eq!(closed.outcomes(), outcomes);

    // Fresh SELECT -> row_to_attempt reconstructs the Passed closure end-to-end.
    let reloaded = attempts.get(&att.id).await.expect("get").expect("present");
    assert_eq!(reloaded.status(), AttemptStatus::Passed);
    assert_eq!(reloaded.fail_reason(), None);
    assert_eq!(reloaded.outcomes(), outcomes);
}

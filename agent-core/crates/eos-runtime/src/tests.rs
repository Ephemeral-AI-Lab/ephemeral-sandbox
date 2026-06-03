//! Phase-6 acceptance-criteria tests for the composition root.
//!
//! In-crate (`#[cfg(test)]`) so they can read the `pub(crate)` DI graph fields
//! and shared test seams directly without widening the public API. Each test
//! names the AC it proves (impl-eos-runtime.md §11).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use eos_agent_def::{AgentDefinition, AgentRegistry, AgentRole};
use eos_engine::EventSource;
use eos_state::{TaskRole, TaskStatus, WorkflowStatus};
use serde_json::json;

use crate::app_state::test_seams::{
    agent_def, build_test_state, factory_from, factory_root_blocks_after, tool_use_turn,
    BlockingSource,
};
use crate::app_state::EventSourceFactory;
use crate::{start_request, AppState};

fn root_agent() -> AgentDefinition {
    agent_def(
        "root",
        AgentRole::Root,
        &["read_file", "delegate_workflow"],
        &["submit_root_outcome"],
    )
}

fn planner_agent() -> AgentDefinition {
    let mut def = agent_def(
        "planner",
        AgentRole::Planner,
        &["read_file"],
        &["submit_planner_outcome"],
    );
    def.context_recipe = Some("planner".to_owned());
    def
}

fn sqlite_url(dir: &std::path::Path) -> String {
    format!("sqlite://{}", dir.join("t.db").display())
}

// --- AC-eos-runtime-04: single-place graph construction + network-url fail-fast.

#[tokio::test]
async fn builder_constructs_all_stores_and_seams() {
    let (state, _dir) = build_test_state(None, vec![root_agent()]).await;
    let req: eos_types::RequestId = "req-build".parse().unwrap();
    state
        .request_store
        .create_request(&req, "/tmp", None, "hi")
        .await
        .unwrap();
    assert!(state.request_store.get(&req).await.unwrap().is_some());
}

#[tokio::test]
async fn network_database_url_fails_fast() {
    let result = AppState::builder()
        .database_url("postgres://localhost/db")
        .build()
        .await;
    assert!(result.is_err(), "a network db url must fail fast");
}

// --- AC-eos-runtime-06: missing model registry is non-fatal.

#[tokio::test]
async fn missing_model_registry_does_not_fail_startup() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::builder()
        .database_url(sqlite_url(dir.path()))
        .model_registry_path(dir.path().join("does-not-exist.json"))
        .build()
        .await;
    assert!(
        state.is_ok(),
        "missing model registry json must be non-fatal"
    );
}

// --- AC-eos-runtime-09: unknown profile tool fails startup (unless compat mode).

#[tokio::test]
async fn unknown_profile_tool_fails_startup() {
    let bad = agent_def(
        "root",
        AgentRole::Root,
        &["totally_not_a_tool"],
        &["submit_root_outcome"],
    );
    let dir = tempfile::tempdir().unwrap();

    let registry: AgentRegistry = vec![bad.clone()].into_iter().collect();
    let result = AppState::builder()
        .database_url(sqlite_url(dir.path()))
        .agent_registry(Arc::new(registry))
        .build()
        .await;
    assert!(result.is_err(), "an unknown tool name must fail startup");

    let registry: AgentRegistry = vec![bad].into_iter().collect();
    let ok = AppState::builder()
        .database_url(sqlite_url(dir.path()))
        .agent_registry(Arc::new(registry))
        .compatibility_mode(true)
        .build()
        .await;
    assert!(
        ok.is_ok(),
        "compatibility mode must skip unknown-tool validation"
    );
}

// --- AC-eos-runtime-01: root request mints a root Task, no root workflow.

#[tokio::test]
async fn start_request_mints_root_task_no_workflow() {
    let factory = factory_from(vec![tool_use_turn(
        "toolu_1",
        "submit_root_outcome",
        json!({"status": "success", "outcome": "done"}),
    )]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let handle = start_request(&state, "do the thing", Some("sb-1"), None)
        .await
        .unwrap();
    let request_id = handle.request_id.clone();
    let root_task_id = handle.root_task_id.clone();
    handle.join().await;

    let task = state
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .expect("root task row exists");
    assert_eq!(task.role, TaskRole::Root);
    assert_eq!(task.workflow_id, None);

    let request = state
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .expect("request row exists");
    assert_eq!(request.root_task_id.as_ref(), Some(&root_task_id));

    let workflows = state
        .workflow_store
        .list_for_parent_task(&root_task_id)
        .await
        .unwrap();
    assert!(workflows.is_empty(), "the root must not create a workflow");
}

// --- AC-eos-runtime-02 / -08: root success keeps the engine-stamped terminal.

#[tokio::test]
async fn successful_root_keeps_engine_terminal() {
    let factory = factory_from(vec![tool_use_turn(
        "toolu_1",
        "submit_root_outcome",
        json!({"status": "success", "outcome": "all done"}),
    )]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let handle = start_request(&state, "task", Some("sb-1"), None)
        .await
        .unwrap();
    let request_id = handle.request_id.clone();
    let root_task_id = handle.root_task_id.clone();
    handle.join().await;

    let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Done);
    let terminal = task.terminal_tool_result.expect("terminal tool result");
    assert_eq!(
        terminal.get("outcome").and_then(|v| v.as_str()),
        Some("all done")
    );
    assert!(
        terminal.get("fail_reason").is_none(),
        "success must not be clobbered by the unfinished-root guard"
    );

    let request = state.request_store.get(&request_id).await.unwrap().unwrap();
    assert_eq!(request.status, "done");
}

// --- AC-eos-runtime-03: an unfinished root fails cleanly with run_exhausted.

#[tokio::test]
async fn unfinished_root_sets_run_exhausted() {
    // An empty factory: the first turn yields no completion → the run ends with
    // terminal_result=None → the unfinished-root guard fires.
    let factory = factory_from(vec![]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let handle = start_request(&state, "task", Some("sb-1"), None)
        .await
        .unwrap();
    let request_id = handle.request_id.clone();
    let root_task_id = handle.root_task_id.clone();
    handle.join().await;

    let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    let terminal = task.terminal_tool_result.expect("terminal tool result");
    assert_eq!(
        terminal.get("fail_reason").and_then(|v| v.as_str()),
        Some("root_run_exhausted")
    );

    let request = state.request_store.get(&request_id).await.unwrap().unwrap();
    assert_eq!(request.status, "failed");
}

// --- AC-eos-runtime-03b: a join error persists a root failure.

#[tokio::test]
async fn join_error_marks_unfinished_root_failed() {
    let factory: EventSourceFactory =
        Arc::new(|_def: &AgentDefinition| Arc::new(BlockingSource) as Arc<dyn EventSource>);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let handle = start_request(&state, "task", Some("sb-1"), None)
        .await
        .unwrap();
    let request_id = handle.request_id.clone();
    let root_task_id = handle.root_task_id.clone();

    // Let the spawned task reach the blocking stream, then abort it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.root_agent_task.abort();
    handle.join().await; // observes a JoinError → runs the still-running guard.

    let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    let request = state.request_store.get(&request_id).await.unwrap().unwrap();
    assert_eq!(request.status, "failed");
}

// --- AC-eos-runtime-05: delegation creates workflow state, parent stays running.

#[tokio::test]
async fn delegate_workflow_leaves_parent_running() {
    // Root delegates once, then blocks (stays running) so the parent is never the
    // one that closes; the planner gets no turns and fails fast, closing the
    // delegated workflow without touching the parent (GC-eos-runtime-03).
    let factory = factory_root_blocks_after(vec![tool_use_turn(
        "toolu_1",
        "delegate_workflow",
        json!({"goal": "do the subwork"}),
    )]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent(), planner_agent()]).await;
    let handle = start_request(&state, "delegate please", Some("sb-1"), None)
        .await
        .unwrap();
    let root_task_id = handle.root_task_id.clone();

    // Poll for the delegated Workflow→Iteration→Attempt to appear.
    let mut workflow_id = None;
    for _ in 0..150 {
        let workflows = state
            .workflow_store
            .list_for_parent_task(&root_task_id)
            .await
            .unwrap();
        if let Some(workflow) = workflows.first() {
            workflow_id = Some(workflow.id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let workflow_id = workflow_id.expect("delegate_workflow must create a Workflow");

    // The workflow owns an iteration and a first attempt.
    let workflow = state
        .workflow_store
        .get(&workflow_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workflow.parent_task_id, root_task_id);
    let iterations = state
        .iteration_store
        .list_for_workflow(&workflow_id)
        .await
        .unwrap();
    assert!(!iterations.is_empty(), "workflow must create an iteration");
    let attempts = state
        .attempt_store
        .list_for_iteration(&iterations[0].id)
        .await
        .unwrap();
    assert!(!attempts.is_empty(), "iteration must create an attempt");

    // Wait for the delegated workflow to actually CLOSE — the planner fails (no
    // turns), the attempt budget (2) exhausts, and the workflow closes Failed.
    // This drives the close path through the runtime wiring (Phase 6 is the
    // integration point), which the "row appeared" check alone never exercises.
    let mut closed = false;
    for _ in 0..200 {
        let workflow = state
            .workflow_store
            .get(&workflow_id)
            .await
            .unwrap()
            .unwrap();
        if workflow.status != WorkflowStatus::Open {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        closed,
        "the delegated workflow must reach a terminal status"
    );

    // GC-eos-runtime-03: the parent root Task is NOT mutated at workflow close —
    // it is still running (the root agent is blocked, not closed by the workflow).
    let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Running,
        "the parent task must remain running after the delegated workflow closes"
    );

    handle
        .shutdown("test complete", Duration::from_secs(2))
        .await;
}

// --- AC-eos-runtime-07: provisioning binds the request sandbox.
//
// The real `origin=workflow` label logic lives in `eos-sandbox-host`'s
// `RequestSandboxProvisioner` (its `fresh_create_spec_has_request_name_and_labels`
// test) because its `ProviderAdapter` is sealed and cannot be mocked here. This
// runtime-level test proves the binding is threaded into the request row for both
// the explicit-id (whitespace-trimmed) and auto-create paths.

#[tokio::test]
async fn provisioning_binds_request_sandbox() {
    let (state, _dir) = build_test_state(Some(factory_from(vec![])), vec![root_agent()]).await;

    let explicit = start_request(&state, "task", Some("  sb-explicit  "), None)
        .await
        .unwrap();
    let request = state
        .request_store
        .get(&explicit.request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        request
            .sandbox_id
            .as_ref()
            .map(eos_types::SandboxId::as_str),
        Some("sb-explicit"),
        "explicit sandbox id is trimmed and bound"
    );
    explicit.join().await;

    let auto = start_request(&state, "task", None, None).await.unwrap();
    let request = state
        .request_store
        .get(&auto.request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        request
            .sandbox_id
            .as_ref()
            .map(eos_types::SandboxId::as_str),
        Some("sb-test"),
        "auto path binds the provisioner-created sandbox id"
    );
    auto.join().await;
}

// --- AC-eos-runtime-08b: shutdown drains background work and persists the root.

#[tokio::test]
async fn shutdown_cancels_drains_and_fails_running_root() {
    let factory: EventSourceFactory =
        Arc::new(|_def: &AgentDefinition| Arc::new(BlockingSource) as Arc<dyn EventSource>);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let handle = start_request(&state, "task", Some("sb-1"), None)
        .await
        .unwrap();
    let request_id = handle.request_id.clone();
    let root_task_id = handle.root_task_id.clone();
    let token = state.shutdown_token();

    // Let the root reach its blocking stream, then shut down with a short grace.
    tokio::time::sleep(Duration::from_millis(40)).await;
    handle
        .shutdown("operator stop", Duration::from_millis(100))
        .await;

    assert!(token.is_cancelled(), "shutdown cancels the token");
    let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    let request = state.request_store.get(&request_id).await.unwrap().unwrap();
    assert_eq!(request.status, "failed");
}

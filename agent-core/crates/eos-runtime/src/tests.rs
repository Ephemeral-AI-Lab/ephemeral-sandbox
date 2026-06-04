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
    agent_def, build_test_state, factory_by_agent, factory_from, factory_root_blocks_after,
    test_tools_root, tool_use_turn, BlockingSource,
};
use crate::app_state::EventSourceFactory;
use crate::{start_request, AppState};

fn root_agent() -> AgentDefinition {
    agent_def(
        "root",
        AgentRole::Root,
        &["read_file", "delegate_workflow", "ask_advisor"],
        &["submit_root_outcome"],
    )
}

/// The advisor helper agent: read-only tools + the (ungated) advisor terminal.
/// Resolved by name in the engine-driven `ask_advisor` run.
fn advisor_agent() -> AgentDefinition {
    agent_def(
        "advisor",
        AgentRole::Helper,
        &["read_file", "glob", "grep"],
        &["submit_advisor_feedback"],
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
        .tools_root(test_tools_root())
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
        .tools_root(test_tools_root())
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
        .tools_root(test_tools_root())
        .agent_registry(Arc::new(registry))
        .build()
        .await;
    assert!(result.is_err(), "an unknown tool name must fail startup");

    let registry: AgentRegistry = vec![bad].into_iter().collect();
    let ok = AppState::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .agent_registry(Arc::new(registry))
        .compatibility_mode(true)
        .build()
        .await;
    assert!(
        ok.is_ok(),
        "compatibility mode must skip unknown-tool validation"
    );
}

#[tokio::test]
async fn plugin_profile_tool_passes_startup_validation() {
    let root = agent_def(
        "root",
        AgentRole::Root,
        &["read_file", "lsp.hover"],
        &["submit_root_outcome"],
    );
    let dir = tempfile::tempdir().unwrap();

    let registry: AgentRegistry = vec![root].into_iter().collect();
    let state = AppState::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .agent_registry(Arc::new(registry))
        .build()
        .await;
    assert!(
        state.is_ok(),
        "catalog plugin tools must be registered without compatibility mode"
    );
}

// --- request_completion NF1: the non-injected build path seeds a registry from
// `agents_dir` so `root` resolves (the shipped-binary seam, exercised without the
// `agent_registry` injection every other test uses). A synthetic profile keeps
// this decoupled from the bundled `.eos-agents` tree and lets the real
// `validate_agent_tools` run (no compatibility_mode).

#[tokio::test]
async fn agents_dir_seeds_registry_so_root_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    // A minimal root profile naming only Rust-registered tools, so no
    // compatibility_mode is needed and the real tool validation still runs.
    std::fs::write(
        profiles.join("root.md"),
        "---\nname: root\ndescription: d\ntool_call_limit: 5\nrole: root\nagent_type: agent\nallowed_tools: [read_file]\nterminals: [submit_root_outcome]\n---\nroot body\n",
    )
    .unwrap();

    let state = AppState::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .agents_dir(profiles)
        .build()
        .await
        .expect("non-injected build with agents_dir must succeed");

    let root = eos_agent_def::AgentName::new("root").unwrap();
    assert!(
        state.agent_registry.get(&root).is_some(),
        "agents_dir build must resolve the root profile without injection"
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
// `submit_root_outcome` is advisor-gated, so success requires a real approving
// advisor verdict in the transcript. The root asks the advisor (a pure tool_use
// turn) and, once the engine-driven advisor agent returns `verdict="approve"`,
// submits its terminal — no injected advisor port (the stateless gate infers the
// verdict from the transcript).

#[tokio::test]
async fn successful_root_keeps_engine_terminal() {
    let payload = json!({"status": "success", "outcome": "all done"});
    let factory = factory_by_agent(vec![
        (
            "root",
            vec![
                tool_use_turn(
                    "toolu_advise",
                    "ask_advisor",
                    json!({"tool_name": "submit_root_outcome", "tool_payload": payload.clone()}),
                ),
                tool_use_turn("toolu_1", "submit_root_outcome", payload),
            ],
        ),
        (
            "advisor",
            vec![tool_use_turn(
                "toolu_fb",
                "submit_advisor_feedback",
                json!({
                    "verdict": "approve",
                    "summary": "Tool selection correct. Payload supported by the work. No residual risks.",
                }),
            )],
        ),
    ]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent(), advisor_agent()]).await;
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

// --- Root is advisor-gated: with no prior `ask_advisor` exchange in the
// transcript, the stateless advisor pre-hook classifies `missing` and refuses
// `submit_root_outcome`, so the run exhausts and the unfinished-root guard fails
// the request. A real advisor `approve` is the prerequisite for root to complete
// (contrast `successful_root_keeps_engine_terminal`, which drives a real advisor).

#[tokio::test]
async fn root_terminal_blocked_without_advisor_approval() {
    let factory = factory_from(vec![tool_use_turn(
        "toolu_1",
        "submit_root_outcome",
        json!({"status": "success", "outcome": "all done"}),
    )]);
    // The root submits directly without ever calling `ask_advisor`, so the gate
    // finds no advisor verdict in the transcript and denies with reason `missing`.
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let handle = start_request(&state, "task", Some("sb-1"), None)
        .await
        .unwrap();
    let request_id = handle.request_id.clone();
    let root_task_id = handle.root_task_id.clone();
    handle.join().await;

    let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Failed,
        "the advisor gate must block the root terminal under the denying stub"
    );
    let terminal = task.terminal_tool_result.expect("terminal tool result");
    assert_eq!(
        terminal.get("fail_reason").and_then(|v| v.as_str()),
        Some("root_run_exhausted")
    );

    let request = state.request_store.get(&request_id).await.unwrap().unwrap();
    assert_eq!(request.status, "failed");
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

// --- D5 closure (attempt_harness-remediation-PLAN §7, primary criterion): the
// recording harness drives a delegated workflow planner -> generator -> reducer
// to AttemptStatus::Passed -> WorkflowStatus::Succeeded through the REAL
// RuntimeAgentRunner (recording port wired) — NOT the QueueRunner/ScriptedRunner
// doubles. This is the proof Path A-recording closes D5 and §5's success cascade
// is reachable in the live runtime. Each workflow terminal
// (submit_planner/generator/reducer_outcome) is advisor-gated (meta.rs), so each
// role first drives a real ask_advisor -> approve exchange in its own transcript,
// exactly like a production run. The root delegates then blocks (parent never
// mutated at close, GC-eos-runtime-03).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delegated_workflow_drives_to_succeeded_via_real_runner() {
    use crate::app_state::test_seams::ScriptedSource;
    use eos_engine::EventSource;

    // One-step plan: a single generator `g1` (bound to the `coder` generator
    // profile) and one reducer `r1` gating on it. Submit payloads carry only
    // {status, outcome}; the task/attempt ids come from the run's metadata.
    let planner_payload = json!({
        "tasks": [{"id": "g1", "agent_name": "coder", "needs": []}],
        "task_specs": {"g1": "implement g1"},
        "reducers": [{"id": "r1", "needs": ["g1"], "prompt": "reduce g1"}],
    });
    let gen_payload = json!({"status": "success", "outcome": "generated g1"});
    let red_payload = json!({"status": "success", "outcome": "reduced"});

    // Each role: ask the advisor for its own terminal, then (with the approve
    // verdict now in its transcript) submit. The advisor profile returns approve.
    let delegate_turn = tool_use_turn(
        "toolu_delegate",
        "delegate_workflow",
        json!({"goal": "do the subwork"}),
    );
    let planner_turns = vec![
        tool_use_turn(
            "toolu_p_advise",
            "ask_advisor",
            json!({"tool_name": "submit_planner_outcome", "tool_payload": planner_payload.clone()}),
        ),
        tool_use_turn("toolu_p_submit", "submit_planner_outcome", planner_payload),
    ];
    let coder_turns = vec![
        tool_use_turn(
            "toolu_g_advise",
            "ask_advisor",
            json!({"tool_name": "submit_generator_outcome", "tool_payload": gen_payload.clone()}),
        ),
        tool_use_turn("toolu_g_submit", "submit_generator_outcome", gen_payload),
    ];
    let reducer_turns = vec![
        tool_use_turn(
            "toolu_r_advise",
            "ask_advisor",
            json!({"tool_name": "submit_reducer_outcome", "tool_payload": red_payload.clone()}),
        ),
        tool_use_turn("toolu_r_submit", "submit_reducer_outcome", red_payload),
    ];
    let advisor_turns = vec![tool_use_turn(
        "toolu_fb",
        "submit_advisor_feedback",
        json!({"verdict": "approve", "summary": "Tool and payload validated. Approve."}),
    )];

    // Root delegates then blocks (stays running); the workflow agents run their
    // scripts; everyone else gets an empty (first-turn-erroring) source.
    let factory: EventSourceFactory = Arc::new(move |def: &AgentDefinition| {
        let turns = match def.name.as_str() {
            "root" => {
                return Arc::new(ScriptedSource::new_blocking(vec![delegate_turn.clone()]))
                    as Arc<dyn EventSource>
            }
            "planner" => planner_turns.clone(),
            "coder" => coder_turns.clone(),
            "reducer" => reducer_turns.clone(),
            "advisor" => advisor_turns.clone(),
            _ => Vec::new(),
        };
        Arc::new(ScriptedSource::new(turns)) as Arc<dyn EventSource>
    });

    let mut planner = agent_def(
        "planner",
        AgentRole::Planner,
        &["ask_advisor", "read_file"],
        &["submit_planner_outcome"],
    );
    planner.context_recipe = Some("planner".to_owned());
    let mut coder = agent_def(
        "coder",
        AgentRole::Generator,
        &["ask_advisor", "read_file"],
        &["submit_generator_outcome"],
    );
    coder.context_recipe = Some("generator".to_owned());
    let mut reducer = agent_def(
        "reducer",
        AgentRole::Reducer,
        &["ask_advisor", "read_file"],
        &["submit_reducer_outcome"],
    );
    reducer.context_recipe = Some("reducer".to_owned());
    let root = agent_def(
        "root",
        AgentRole::Root,
        &["delegate_workflow", "ask_advisor", "read_file"],
        &["submit_root_outcome"],
    );

    let (state, _dir) = build_test_state(
        Some(factory),
        vec![root, planner, coder, reducer, advisor_agent()],
    )
    .await;
    let handle = start_request(&state, "delegate please", Some("sb-1"), None)
        .await
        .unwrap();
    let root_task_id = handle.root_task_id.clone();

    // The delegated workflow appears, then drives to Succeeded entirely through
    // the real RuntimeAgentRunner + recording port.
    let mut workflow_id = None;
    for _ in 0..200 {
        if let Some(workflow) = state
            .workflow_store
            .list_for_parent_task(&root_task_id)
            .await
            .unwrap()
            .first()
        {
            workflow_id = Some(workflow.id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let workflow_id = workflow_id.expect("delegate_workflow must create a Workflow");

    let mut final_status = None;
    for _ in 0..500 {
        let status = state
            .workflow_store
            .get(&workflow_id)
            .await
            .unwrap()
            .unwrap()
            .status;
        if status != WorkflowStatus::Open {
            final_status = Some(status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        final_status,
        Some(WorkflowStatus::Succeeded),
        "the recording harness must drive the delegated workflow to Succeeded via the real runner"
    );

    // The success cascade reached every level: attempt PASSED, iteration
    // SUCCEEDED, and the parent root Task is untouched (still running).
    let iterations = state
        .iteration_store
        .list_for_workflow(&workflow_id)
        .await
        .unwrap();
    let attempts = state
        .attempt_store
        .list_for_iteration(&iterations[0].id)
        .await
        .unwrap();
    assert_eq!(attempts[0].status, eos_state::AttemptStatus::Passed);
    assert_eq!(iterations[0].status, eos_state::IterationStatus::Succeeded);
    assert_eq!(
        state
            .task_store
            .get(&root_task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running,
        "the parent root task must remain running after the delegated workflow succeeds"
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

// --- AC-eos-runtime-08b: shutdown cancels background work and persists the root.

#[tokio::test]
async fn shutdown_cancels_background_and_fails_running_root() {
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

// --- Slice 1 instance identity (anchor §7): a backgrounded command-session
// completion is pulled by the per-request heartbeat and delivered to the model
// as a `[BACKGROUND COMPLETED]` SystemNotification in a later provider request.
// This goes through the real `start_request` wiring, proving the heartbeat sink
// and the loop's `notifier` are the SAME `NotificationService`. If the wiring
// handed the loop a different instance, the model would never see the
// notification and `saw_notification` would stay false.
mod command_session_delivery {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use eos_agent_def::{AgentRegistry, AgentRole};
    use eos_engine::{
        AssistantMessageComplete, EngineError, EngineStream, EventSource, StreamEvent,
    };
    use eos_llm_client::{ContentBlock, LlmRequest, Message, MessageRole, UsageSnapshot};
    use eos_sandbox_api::{DaemonOp, SandboxApiError, SandboxTransport};
    use eos_state::TaskStatus;
    use eos_types::{JsonObject, SandboxId};
    use serde_json::json;

    use crate::app_state::test_seams::{agent_def, FakeProvisioner, ScriptedSource};
    use crate::app_state::EventSourceFactory;
    use crate::{start_request, AppState};

    /// A fake daemon transport: `exec_command` starts a backgrounded session
    /// `cmd_1`, `collect_completed` parks a successful completion for it, and the
    /// terminal-gate count is 0.
    #[derive(Debug, Default)]
    struct CommandCompletionTransport;

    #[async_trait]
    impl SandboxTransport for CommandCompletionTransport {
        async fn call(
            &self,
            _sandbox_id: &SandboxId,
            op: DaemonOp,
            _payload: JsonObject,
            _timeout_s: u32,
        ) -> Result<JsonObject, SandboxApiError> {
            let value = match op {
                DaemonOp::ExecCommand => json!({
                    "status": "running",
                    "command_session_id": "cmd_1",
                    "output": {"stdout": "", "stderr": ""},
                }),
                DaemonOp::CommandCollectCompleted => json!({
                    "success": true,
                    "completions": [{
                        "command_session_id": "cmd_1",
                        "agent_id": "root",
                        "command": "sleep 1",
                        "result": {
                            "status": "ok",
                            "exit_code": 0,
                            "output": {"stdout": "background done", "stderr": ""},
                        },
                    }],
                }),
                DaemonOp::CommandSessionCount => json!({"success": true, "count": 0}),
                _ => json!({}),
            };
            Ok(value.as_object().cloned().unwrap_or_default())
        }
    }

    fn stream_of(events: Vec<StreamEvent>) -> EngineStream {
        Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
    }

    fn tool_turn(tool_use_id: &str, name: &str, input: serde_json::Value) -> Vec<StreamEvent> {
        let input = match input {
            serde_json::Value::Object(map) => map,
            _ => JsonObject::new(),
        };
        vec![StreamEvent::AssistantMessageComplete {
            agent_name: String::new(),
            agent_run_id: None,
            payload: Box::new(AssistantMessageComplete {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        tool_use_id: tool_use_id.parse().expect("tool use id"),
                        name: name.to_owned(),
                        input,
                    }],
                },
                usage: UsageSnapshot::default(),
                stop_reason: None,
            }),
        }]
    }

    fn text_turn(text: &str) -> Vec<StreamEvent> {
        vec![StreamEvent::AssistantMessageComplete {
            agent_name: String::new(),
            agent_run_id: None,
            payload: Box::new(AssistantMessageComplete {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: text.to_owned(),
                    }],
                },
                usage: UsageSnapshot::default(),
                stop_reason: None,
            }),
        }]
    }

    /// Drives the root: turn 1 launches a background command session; it then
    /// returns text turns until the `[BACKGROUND COMPLETED]` notification for
    /// `cmd_1` lands in the transcript. Because `submit_root_outcome` is
    /// advisor-gated, it then asks the advisor (one turn) and, on the following
    /// turn — with the approve verdict now in the transcript — submits its
    /// terminal (recording that it saw the notification).
    struct DeliveryProbeSource {
        started: Arc<AtomicBool>,
        asked_advisor: Arc<AtomicBool>,
        saw_notification: Arc<AtomicBool>,
    }

    #[async_trait]
    impl EventSource for DeliveryProbeSource {
        async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
            let seen = request.messages.iter().any(|message| {
                message.content.iter().any(|block| {
                    matches!(block, ContentBlock::SystemNotification { text }
                        if text.contains("[BACKGROUND COMPLETED]") && text.contains("cmd_1"))
                })
            });
            if seen {
                self.saw_notification.store(true, Ordering::SeqCst);
                if !self.asked_advisor.swap(true, Ordering::SeqCst) {
                    return Ok(stream_of(tool_turn(
                        "toolu_advise",
                        "ask_advisor",
                        json!({
                            "tool_name": "submit_root_outcome",
                            "tool_payload": {"status": "success", "outcome": "saw background completion"},
                        }),
                    )));
                }
                return Ok(stream_of(tool_turn(
                    "toolu_done",
                    "submit_root_outcome",
                    json!({"status": "success", "outcome": "saw background completion"}),
                )));
            }
            if !self.started.swap(true, Ordering::SeqCst) {
                return Ok(stream_of(tool_turn(
                    "toolu_exec",
                    "exec_command",
                    json!({"cmd": "sleep 1"}),
                )));
            }
            // Yield so the per-request heartbeat can pull and enqueue the parked
            // completion before the next loop-top drain.
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(stream_of(text_turn("waiting for the background command")))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backgrounded_completion_lands_as_system_notification() {
        // A fast heartbeat keeps the test sub-second instead of waiting ~1 s.
        std::env::set_var("EOS_COMMAND_HEARTBEAT_MS", "20");

        let mut root = agent_def(
            "root",
            AgentRole::Root,
            &["exec_command", "read_file", "ask_advisor"],
            &["submit_root_outcome"],
        );
        // Generous budget so the wait-loop never trips the no-terminal ceiling
        // before the (fast) heartbeat delivers.
        root.tool_call_limit = NonZeroU32::new(40).expect("nonzero");
        let advisor = agent_def(
            "advisor",
            AgentRole::Helper,
            &["read_file", "glob", "grep"],
            &["submit_advisor_feedback"],
        );

        let started = Arc::new(AtomicBool::new(false));
        let asked_advisor = Arc::new(AtomicBool::new(false));
        let saw_notification = Arc::new(AtomicBool::new(false));
        let started_factory = started.clone();
        let asked_factory = asked_advisor.clone();
        let saw_factory = saw_notification.clone();
        // The advisor agent runs a real approve turn; the root probe drives the
        // rest. No injected advisor port — the gate reads the transcript.
        let factory: EventSourceFactory = Arc::new(move |def| {
            if def.name.as_str() == "advisor" {
                Arc::new(ScriptedSource::new(vec![tool_turn(
                    "toolu_fb",
                    "submit_advisor_feedback",
                    json!({"verdict": "approve", "summary": "background completion is real; approve"}),
                )])) as Arc<dyn EventSource>
            } else {
                Arc::new(DeliveryProbeSource {
                    started: started_factory.clone(),
                    asked_advisor: asked_factory.clone(),
                    saw_notification: saw_factory.clone(),
                }) as Arc<dyn EventSource>
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        let registry: AgentRegistry = vec![root, advisor].into_iter().collect();
        let state = AppState::builder()
            .database_url(url)
            .cwd(dir.path().display().to_string())
            .tools_root(crate::app_state::test_seams::test_tools_root())
            .provisioner(Arc::new(FakeProvisioner {
                id: "sb-test".to_owned(),
            }))
            .transport(Arc::new(CommandCompletionTransport))
            .agent_registry(Arc::new(registry))
            .event_source_factory(factory)
            .build()
            .await
            .expect("build state");

        let handle = start_request(&state, "run a background command", Some("sb-test"), None)
            .await
            .expect("start request");
        let root_task_id = handle.root_task_id.clone();
        handle.join().await;

        assert!(
            saw_notification.load(Ordering::SeqCst),
            "the backgrounded completion must reach the model as a SystemNotification \
             (heartbeat sink and loop notifier must be the same instance)"
        );
        let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "seeing the notification lets the root submit its terminal"
        );
    }
}

// --- Subagent lifecycle (subagent-remediation-PLAN §5): a real child agent runs
// through `run_ephemeral_agent`, its result surfaces as `finished`, and a live
// subagent is drained (not wedged) at the parent's terminal (D1/D2/D3/D9).
mod subagent_lifecycle {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use eos_agent_def::{AgentDefinition, AgentRole, AgentType};
    use eos_engine::{EngineError, EngineStream, EventSource, StreamEvent};
    use eos_llm_client::{ContentBlock, LlmRequest};
    use eos_state::TaskStatus;
    use serde_json::json;

    use crate::app_state::test_seams::{
        agent_def, build_test_state, tool_use_turn, ScriptedSource,
    };
    use crate::app_state::EventSourceFactory;
    use crate::start_request;

    fn stream_of(events: Vec<StreamEvent>) -> EngineStream {
        Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
    }

    fn root_with_subagent() -> AgentDefinition {
        let mut def = agent_def(
            "root",
            AgentRole::Root,
            &[
                "run_subagent",
                "check_subagent_progress",
                "read_file",
                "ask_advisor",
            ],
            &["submit_root_outcome"],
        );
        // Generous budget so the poll loop never trips the no-terminal ceiling.
        def.tool_call_limit = NonZeroU32::new(40).expect("nonzero");
        def
    }

    fn explorer_subagent() -> AgentDefinition {
        let mut def = agent_def(
            "explorer",
            AgentRole::Helper,
            &["read_file"],
            &["submit_exploration_result"],
        );
        def.agent_type = AgentType::Subagent;
        def
    }

    fn advisor_agent() -> AgentDefinition {
        agent_def(
            "advisor",
            AgentRole::Helper,
            &["read_file", "glob", "grep"],
            &["submit_advisor_feedback"],
        )
    }

    fn approve_turn() -> Vec<StreamEvent> {
        tool_use_turn(
            "toolu_fb",
            "submit_advisor_feedback",
            json!({"verdict": "approve", "summary": "subagent path validated; approve"}),
        )
    }

    // D9: a *live* subagent (its run blocks forever) must NOT wedge the root
    // terminal. The `submit_root_outcome` prehook cancels it (settle + abort), so
    // the root completes; the old deny-if-count>0 path would have failed the root.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_root_outcome_cancels_live_subagent() {
        let payload = json!({"status": "success", "outcome": "done despite a live subagent"});
        let root_turns = vec![
            tool_use_turn(
                "toolu_sub",
                "run_subagent",
                json!({"agent_name": "explorer", "prompt": "investigate forever"}),
            ),
            tool_use_turn(
                "toolu_advise",
                "ask_advisor",
                json!({"tool_name": "submit_root_outcome", "tool_payload": payload.clone()}),
            ),
            tool_use_turn("toolu_root", "submit_root_outcome", payload.clone()),
        ];
        let advisor_turns = vec![approve_turn()];
        let factory: EventSourceFactory = Arc::new(move |def: &AgentDefinition| {
            match def.name.as_str() {
                // The explorer never finishes → its record stays Running.
                "explorer" => {
                    Arc::new(ScriptedSource::new_blocking(Vec::new())) as Arc<dyn EventSource>
                }
                "advisor" => {
                    Arc::new(ScriptedSource::new(advisor_turns.clone())) as Arc<dyn EventSource>
                }
                _ => Arc::new(ScriptedSource::new(root_turns.clone())) as Arc<dyn EventSource>,
            }
        });

        let (state, _dir) = build_test_state(
            Some(factory),
            vec![root_with_subagent(), advisor_agent(), explorer_subagent()],
        )
        .await;
        let handle = start_request(&state, "task", Some("sb-1"), None)
            .await
            .unwrap();
        let root_task_id = handle.root_task_id.clone();
        let supervisor = handle.supervisor.clone();
        handle.join().await;

        let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "a live subagent must not wedge the root terminal — the prehook cancels it (D9)"
        );
        // The cancellation settled the live subagent: no Running subagent remains.
        assert_eq!(
            supervisor.inner().lock().await.inflight_report("").subagent,
            0,
            "cancellation must leave zero in-flight subagents"
        );
    }

    /// A root probe: launch the subagent, then poll `check_subagent_progress`
    /// until the transcript shows the child `finished`, then approve + submit.
    struct FinishProbeSource {
        started: Arc<AtomicBool>,
        asked_advisor: Arc<AtomicBool>,
        saw_finished: Arc<AtomicBool>,
    }

    #[async_trait]
    impl EventSource for FinishProbeSource {
        async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
            let finished = request.messages.iter().any(|message| {
                message.content.iter().any(|block| {
                    matches!(block, ContentBlock::ToolResult { content, .. }
                        if content.contains("\"status\": \"finished\""))
                })
            });
            if finished {
                self.saw_finished.store(true, Ordering::SeqCst);
                let payload = json!({"status": "success", "outcome": "subagent finished"});
                if !self.asked_advisor.swap(true, Ordering::SeqCst) {
                    return Ok(stream_of(tool_use_turn(
                        "toolu_advise",
                        "ask_advisor",
                        json!({"tool_name": "submit_root_outcome", "tool_payload": payload}),
                    )));
                }
                return Ok(stream_of(tool_use_turn(
                    "toolu_root",
                    "submit_root_outcome",
                    payload,
                )));
            }
            if !self.started.swap(true, Ordering::SeqCst) {
                return Ok(stream_of(tool_use_turn(
                    "toolu_sub",
                    "run_subagent",
                    json!({"agent_name": "explorer", "prompt": "investigate"}),
                )));
            }
            // Yield so the spawned subagent run can reach its terminal before the
            // next check.
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(stream_of(tool_use_turn(
                "toolu_check",
                "check_subagent_progress",
                json!({"subagent_session_id": "subagent_1", "last_n_messages": 5}),
            )))
        }
    }

    // D1/D3 end-to-end: a real explorer child runs, calls
    // `submit_exploration_result`, and `check_subagent_progress` reports
    // `finished` — no test-only fake supervisor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_runs_and_reports_finished() {
        let saw_finished = Arc::new(AtomicBool::new(false));
        let saw_finished_factory = saw_finished.clone();
        let explorer_turns = vec![tool_use_turn(
            "toolu_expl",
            "submit_exploration_result",
            json!({"summary": "the bug is at foo.rs:10", "findings": ["foo.rs:10"]}),
        )];
        let advisor_turns = vec![approve_turn()];
        let factory: EventSourceFactory =
            Arc::new(move |def: &AgentDefinition| match def.name.as_str() {
                "explorer" => {
                    Arc::new(ScriptedSource::new(explorer_turns.clone())) as Arc<dyn EventSource>
                }
                "advisor" => {
                    Arc::new(ScriptedSource::new(advisor_turns.clone())) as Arc<dyn EventSource>
                }
                _ => Arc::new(FinishProbeSource {
                    started: Arc::new(AtomicBool::new(false)),
                    asked_advisor: Arc::new(AtomicBool::new(false)),
                    saw_finished: saw_finished_factory.clone(),
                }) as Arc<dyn EventSource>,
            });

        let (state, _dir) = build_test_state(
            Some(factory),
            vec![root_with_subagent(), advisor_agent(), explorer_subagent()],
        )
        .await;
        let handle = start_request(&state, "task", Some("sb-1"), None)
            .await
            .unwrap();
        let root_task_id = handle.root_task_id.clone();
        handle.join().await;

        assert!(
            saw_finished.load(Ordering::SeqCst),
            "the child explorer must run and report finished via check_subagent_progress"
        );
        let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "the root completes after the subagent finishes"
        );
    }

    /// Replays scripted root turns, capturing any `run_subagent` rejection text
    /// from the transcript so the test can assert the Python error message.
    struct RejectionProbe {
        turns: std::sync::Mutex<Vec<Vec<StreamEvent>>>,
        rejection: Arc<std::sync::Mutex<Option<String>>>,
    }

    #[async_trait]
    impl EventSource for RejectionProbe {
        async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
            for message in &request.messages {
                for block in &message.content {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        if content.starts_with("run_subagent: agent") {
                            *self.rejection.lock().unwrap() = Some(content.clone());
                        }
                    }
                }
            }
            let mut turns = self.turns.lock().unwrap();
            if turns.is_empty() {
                return Ok(stream_of(Vec::new()));
            }
            Ok(stream_of(turns.remove(0)))
        }
    }

    // D2: an unknown dispatch is rejected in-band with the Python message and
    // mints no record, while the root still completes (the rejection is an
    // in-band tool error, not a wedge).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_subagent_is_rejected_in_band() {
        let payload = json!({"status": "success", "outcome": "rejected cleanly"});
        let root_turns = vec![
            tool_use_turn(
                "toolu_sub",
                "run_subagent",
                json!({"agent_name": "explorer", "prompt": "go"}),
            ),
            tool_use_turn(
                "toolu_advise",
                "ask_advisor",
                json!({"tool_name": "submit_root_outcome", "tool_payload": payload.clone()}),
            ),
            tool_use_turn("toolu_root", "submit_root_outcome", payload.clone()),
        ];
        let advisor_turns = vec![approve_turn()];
        let rejection: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let rejection_probe = rejection.clone();
        let factory: EventSourceFactory =
            Arc::new(move |def: &AgentDefinition| match def.name.as_str() {
                "advisor" => {
                    Arc::new(ScriptedSource::new(advisor_turns.clone())) as Arc<dyn EventSource>
                }
                _ => Arc::new(RejectionProbe {
                    turns: std::sync::Mutex::new(root_turns.clone()),
                    rejection: rejection_probe.clone(),
                }) as Arc<dyn EventSource>,
            });

        // No "explorer" agent registered → run_subagent must reject "not registered".
        let (state, _dir) =
            build_test_state(Some(factory), vec![root_with_subagent(), advisor_agent()]).await;
        let handle = start_request(&state, "task", Some("sb-1"), None)
            .await
            .unwrap();
        let root_task_id = handle.root_task_id.clone();
        handle.join().await;

        let captured = rejection.lock().unwrap().clone();
        assert_eq!(
            captured.as_deref(),
            Some("run_subagent: agent 'explorer' is not registered."),
            "an unregistered agent is rejected with the Python error text"
        );
        let task = state.task_store.get(&root_task_id).await.unwrap().unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "an in-band rejection does not wedge the root"
        );
    }
}

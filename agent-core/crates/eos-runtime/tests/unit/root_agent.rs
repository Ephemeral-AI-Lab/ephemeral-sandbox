use super::*;

// --- AC-eos-runtime-04: single-place graph construction + network-url fail-fast.

#[tokio::test]
async fn builder_constructs_all_stores_and_seams() {
    let (state, _dir) = build_test_state(None, vec![root_agent()]).await;
    let req: eos_types::RequestId = "req-build".parse().unwrap();
    state
        .db
        .request_store
        .create_request(&req, "/tmp", None, "hi")
        .await
        .unwrap();
    assert!(state.db.request_store.get(&req).await.unwrap().is_some());
}

// AC-eos-runtime / AC9: the backend composition root reads agent-core state only
// through `RuntimeServices::state_reader()`. Prove the seam returns live store
// handles (pointing at the runtime's own DB, not empty stubs) and exposes the
// read-side list APIs by seeding and reading back entirely through the reader.
#[tokio::test]
async fn state_reader_exposes_live_request_task_and_run_stores() {
    let (state, _dir) = build_test_state(None, vec![root_agent()]).await;
    let reader = state.state_reader();

    let request_id: RequestId = "req-reader".parse().unwrap();
    reader
        .requests()
        .create_request(&request_id, "/w", None, "reader prompt")
        .await
        .unwrap();
    let task = Task {
        id: "t-reader".parse().unwrap(),
        request_id: request_id.clone(),
        role: TaskRole::Root,
        instruction: "do it".to_owned(),
        status: TaskStatus::Running,
        workflow_id: None,
        iteration_id: None,
        attempt_id: None,
        agent_name: Some("root".to_owned()),
        needs: Vec::new(),
        outcomes: Vec::new(),
        terminal_tool_result: None,
    };
    reader.tasks().insert_task(&task).await.unwrap();
    let run_id: AgentRunId = "run-reader".parse().unwrap();
    reader
        .agent_runs()
        .create_run(&run_id, Some(&task.id), "root", None)
        .await
        .unwrap();

    // Reads return through the same reader handles: request listing (with the
    // unwindowed total), the per-request task tree, and the latest run for a task.
    let listed = reader
        .requests()
        .list(RequestListFilter::default(), Page::default())
        .await
        .unwrap();
    assert_eq!(listed.total, 1);
    assert_eq!(listed.items[0].id, request_id);
    let tasks = reader.tasks().list_for_request(&request_id).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, task.id);
    let run = reader.agent_runs().get_for_task(&task.id).await.unwrap();
    assert_eq!(run.unwrap().id, run_id);
}

#[tokio::test]
async fn network_database_url_fails_fast() {
    let result = RuntimeServices::builder()
        .database_url("postgres://localhost/db")
        .tools_root(test_tools_root())
        .build()
        .await;
    assert!(result.is_err(), "a network db url must fail fast");
}

// --- AC-eos-runtime-06: model registry seeds from provider-nested config.

#[tokio::test]
async fn provider_models_seed_model_registry() {
    let dir = tempfile::tempdir().unwrap();
    let db_url = sqlite_url(dir.path());
    let providers: ProvidersConfig = serde_json::from_value(json!({
        "active": "codex_coding_plan",
        "codex_coding_plan": {
            "access_token": "unused-token",
            "models": {
                "active": "gpt-5.5",
                "registrations": [
                    { "key": "gpt-5.5", "label": "Codex GPT-5.5", "kwargs": { "reasoning_effort": "medium" } }
                ]
            }
        }
    }))
    .unwrap();

    let _state = RuntimeServices::builder()
        .database_url(db_url.clone())
        .tools_root(test_tools_root())
        .providers_config(providers)
        .llm_client(Arc::new(NoopLlmClient))
        .build()
        .await
        .unwrap();

    let mut db_config = DatabaseConfig::default();
    db_config.url = DatabaseUrl::parse(db_url).unwrap();
    let db = Database::open(&db_config).await.unwrap();
    let active = db
        .model_registry()
        .active_resolved()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.model_key, "gpt-5.5");
    assert_eq!(
        active.kwargs.get("reasoning_effort"),
        Some(&json!("medium"))
    );
}

#[tokio::test]
async fn workflow_config_wires_attempt_and_planner_depth() {
    let dir = tempfile::tempdir().unwrap();
    let mut workflow = WorkflowConfig::default();
    workflow.max_depth = 3;
    workflow.attempt.max_concurrent_task_runs = 5;

    let state = RuntimeServices::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .workflow_config(workflow)
        .build()
        .await
        .unwrap();

    assert!(state
        .agent_core
        .tool_config
        .get(ToolName::SubmitPlannerOutcome)
        .hooks
        .contains(&Hook::DisallowNestedPlannerDeferral {
            tool: ToolName::SubmitPlannerOutcome,
            max_depth: 3,
        }));
}

// --- AC-eos-runtime-09: unknown profile tool fails startup.

#[tokio::test]
async fn unknown_profile_tool_fails_startup() {
    let bad = agent_def("root", &["totally_not_a_tool"], &["submit_root_outcome"]);
    let dir = tempfile::tempdir().unwrap();

    let registry: AgentRegistry = vec![bad].into_iter().collect();
    let result = RuntimeServices::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .agent_registry(Arc::new(registry))
        .build()
        .await;
    assert!(result.is_err(), "an unknown tool name must fail startup");
}

#[tokio::test]
async fn plugin_profile_tool_passes_startup_validation() {
    let root = agent_def(
        "root",
        &["read_file", "lsp.hover"],
        &["submit_root_outcome"],
    );
    let dir = tempfile::tempdir().unwrap();

    let registry: AgentRegistry = vec![root].into_iter().collect();
    let state = RuntimeServices::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .agent_registry(Arc::new(registry))
        .build()
        .await;
    assert!(
        state.is_ok(),
        "catalog plugin tools must be registered during startup validation"
    );
}

// --- request_completion NF1: the non-injected build path seeds a registry from
// `agents_dir` so `root` resolves (the shipped-binary seam, exercised without the
// `agent_registry` injection every other test uses). A synthetic profile keeps
// this decoupled from the bundled `.eos-agents` tree and lets the real
// `validate_agent_tools` run.

#[tokio::test]
async fn agents_dir_seeds_registry_so_root_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    // A minimal root profile naming only Rust-registered tools, so the real tool
    // validation still runs.
    std::fs::write(
        profiles.join("root.md"),
        "---\nname: root\ndescription: d\ntool_call_limit: 5\nagent_type: agent\nallowed_tools: [read_file]\nterminals: [submit_root_outcome]\n---\nroot body\n",
    )
    .unwrap();

    let state = RuntimeServices::builder()
        .database_url(sqlite_url(dir.path()))
        .tools_root(test_tools_root())
        .agents_dir(profiles)
        .build()
        .await
        .expect("non-injected build with agents_dir must succeed");

    let root = eos_types::AgentName::new("root").unwrap();
    assert!(
        state.agent_core.agent_registry.get(&root).is_some(),
        "agents_dir build must resolve the root profile without injection"
    );
}

// --- AC-eos-runtime-01: root request mints a root Task, no root workflow.

#[tokio::test]
async fn run_request_mints_root_task_no_workflow() {
    let factory = factory_from(vec![tool_use_turn(
        "toolu_1",
        "submit_root_outcome",
        json!({"status": "success", "outcome": "done"}),
    )]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    // Identity is an input: the caller mints the request id and derives the root
    // task id before the run completes (Option A).
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    let outcome = run_request(&state, &request_id, "do the thing", Some("sb-1"), None)
        .await
        .unwrap();

    let task = state
        .db
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .expect("root task row exists");
    assert_eq!(task.role, TaskRole::Root);
    assert_eq!(task.workflow_id, None);
    // The returned outcome is the single read-back of the persisted root task
    // (step 7 contract): it mirrors the store regardless of success/failure.
    assert_eq!(outcome.status, task.status);
    assert_eq!(outcome.terminal, task.terminal_tool_result);

    let request = state
        .db
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .expect("request row exists");
    assert_eq!(request.root_task_id.as_ref(), Some(&root_task_id));

    let workflows = state
        .db
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
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    run_request(&state, &request_id, "task", Some("sb-1"), None)
        .await
        .unwrap();

    let task = state
        .db
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .unwrap();
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

    let request = state
        .db
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.status, RequestStatus::Done);
}

#[tokio::test]
async fn root_run_writes_runner_owned_message_records() {
    let payload = json!({"status": "success", "outcome": "recorded"});
    let factory = factory_by_agent(vec![
        (
            "root",
            vec![
                tool_use_turn(
                    "toolu_advise",
                    "ask_advisor",
                    json!({"tool_name": "submit_root_outcome", "tool_payload": payload.clone()}),
                ),
                tool_use_turn("toolu_submit", "submit_root_outcome", payload),
            ],
        ),
        (
            "advisor",
            vec![tool_use_turn(
                "toolu_feedback",
                "submit_advisor_feedback",
                json!({
                    "verdict": "approve",
                    "summary": "Tool selection correct. Payload supported by the work. No residual risks.",
                }),
            )],
        ),
    ]);
    let (state, _dir) =
        build_test_state_with_message_records(Some(factory), vec![root_agent(), advisor_agent()])
            .await;
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);

    run_request(
        &state,
        &request_id,
        "message record task",
        Some("sb-1"),
        None,
    )
    .await
    .unwrap();

    let agent_run = state
        .db
        .agent_run_store
        .get_for_task(&root_task_id)
        .await
        .unwrap()
        .expect("root agent run row exists");
    let records = state.message_records().expect("message records configured");
    let events = records.read_events(&agent_run.id, 0).await.unwrap();
    let event_kinds: Vec<_> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(event_kinds.first(), Some(&"node_started"));
    assert_eq!(event_kinds.get(1), Some(&"messages_initialized"));
    assert!(
        event_kinds.contains(&"messages_appended"),
        "root run must append loop-produced messages"
    );
    assert_eq!(event_kinds.last(), Some(&"node_finished"));
    assert_eq!(
        events[0]
            .payload
            .get("type")
            .and_then(|value| value.as_str()),
        Some("root_agent")
    );
    assert_eq!(
        events
            .last()
            .expect("node_finished event")
            .payload
            .get("status")
            .and_then(|value| value.as_str()),
        Some("completed")
    );

    let bytes = records.read_messages(&agent_run.id, 0).await.unwrap();
    let raw = String::from_utf8(bytes.bytes).unwrap();
    let rows: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(rows.iter().any(|row| {
        row.get("type").and_then(|value| value.as_str()) == Some("initial_message")
            && row.get("role").and_then(|value| value.as_str()) == Some("system")
            && row.to_string().contains("test profile")
    }));
    assert!(rows.iter().any(|row| {
        row.get("type").and_then(|value| value.as_str()) == Some("initial_message")
            && row.get("role").and_then(|value| value.as_str()) == Some("user")
            && row.to_string().contains("message record task")
    }));
    assert!(rows.iter().any(|row| {
        row.get("type").and_then(|value| value.as_str()) == Some("message")
            && row.to_string().contains("submit_root_outcome")
    }));
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
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    run_request(&state, &request_id, "task", Some("sb-1"), None)
        .await
        .unwrap();

    let task = state
        .db
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .unwrap();
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

    let request = state
        .db
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.status, RequestStatus::Failed);
}

// --- AC-eos-runtime-03: an unfinished root fails cleanly with run_exhausted.

#[tokio::test]
async fn unfinished_root_sets_run_exhausted() {
    // An empty factory: the first turn yields no completion → the run ends with
    // submission_outcome=None → the unfinished-root guard fires.
    let factory = factory_from(vec![]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent()]).await;
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    run_request(&state, &request_id, "task", Some("sb-1"), None)
        .await
        .unwrap();

    let task = state
        .db
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    let terminal = task.terminal_tool_result.expect("terminal tool result");
    assert_eq!(
        terminal.get("fail_reason").and_then(|v| v.as_str()),
        Some("root_run_exhausted")
    );

    let request = state
        .db
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.status, RequestStatus::Failed);
}

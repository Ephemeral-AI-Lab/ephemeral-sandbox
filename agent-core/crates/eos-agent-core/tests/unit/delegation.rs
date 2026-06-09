use super::*;

// --- AC-eos-agent-core-05: delegation creates workflow state, parent stays running.

#[tokio::test]
async fn delegate_workflow_leaves_parent_running() {
    // Root delegates once, then blocks (stays running) so the parent is never the
    // one that closes; the planner gets no turns and fails fast, closing the
    // delegated workflow without touching the parent (GC-eos-agent-core-03).
    let factory = factory_root_blocks_after(vec![tool_use_turn(
        "toolu_1",
        "delegate_workflow",
        json!({"goal": "do the subwork"}),
    )]);
    let (state, _dir) = build_test_state(Some(factory), vec![root_agent(), planner_agent()]).await;
    // The root blocks forever after delegating, so run it on a spawned task and
    // observe the delegated workflow through the stores (the ids are caller-known
    // via Option A, before the run completes); the abort at the end is teardown.
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    let run = tokio::spawn({
        let state = state.clone();
        let request_id = request_id.clone();
        async move { run_request(&state, &request_id, "delegate please", Some("sb-1"), None).await }
    });

    // Poll for the delegated Workflow→Iteration→Attempt to appear.
    let mut workflow_id = None;
    for _ in 0..150 {
        let workflows = state
            .db
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
        .db
        .workflow_store
        .get(&workflow_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workflow.parent_task_id, root_task_id);
    let mut iterations = Vec::new();
    for _ in 0..150 {
        iterations = state
            .db
            .iteration_store
            .list_for_workflow(&workflow_id)
            .await
            .unwrap();
        if !iterations.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!iterations.is_empty(), "workflow must create an iteration");
    let mut attempts = Vec::new();
    for _ in 0..150 {
        attempts = state
            .db
            .attempt_store
            .list_for_iteration(&iterations[0].id)
            .await
            .unwrap();
        if !attempts.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!attempts.is_empty(), "iteration must create an attempt");

    // Wait for the delegated workflow to actually CLOSE — the planner fails (no
    // turns), the attempt budget (2) exhausts, and the workflow closes Failed.
    // This drives the close path through the runtime wiring (Phase 6 is the
    // integration point), which the "row appeared" check alone never exercises.
    let mut closed = false;
    for _ in 0..200 {
        let workflow = state
            .db
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

    // GC-eos-agent-core-03: the parent root Task is NOT mutated at workflow close —
    // it is still running (the root agent is blocked, not closed by the workflow).
    let task = state
        .db
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Running,
        "the parent task must remain running after the delegated workflow closes"
    );

    // Teardown only: the aborted future runs no finalizer (there is no Drop guard),
    // so every assertion above observed pre-abort state (AC3.3).
    run.abort();
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
// mutated at close, GC-eos-agent-core-03).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delegated_workflow_drives_to_succeeded_via_real_runner() {
    use eos_testkit::ScriptedSource;

    // Each role: ask the advisor for its own terminal, then (with the approve
    // verdict now in its transcript) submit. The advisor profile returns approve.
    let delegate_turn = tool_use_turn(
        "toolu_delegate",
        "delegate_workflow",
        json!({"goal": "do the subwork"}),
    );
    let (planner_turns, coder_turns, reducer_turns) = one_step_workflow_turns();
    let advisor_turns = vec![tool_use_turn(
        "toolu_fb",
        "submit_advisor_feedback",
        json!({"verdict": "approve", "summary": "Tool and payload validated. Approve."}),
    )];

    // Root delegates then blocks (stays running); the workflow agents run their
    // scripts; everyone else gets an empty (first-turn-erroring) source.
    let factory: ProviderStreamSourceFactory =
        Arc::new(
            move |_request, agent_state| match agent_state.agent_name.as_str() {
                "root" => Arc::new(ScriptedSource::new_blocking(vec![delegate_turn.clone()]))
                    as Arc<dyn ProviderStreamSource>,
                "planner" => Arc::new(ScriptedSource::new(planner_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                "coder" => Arc::new(ScriptedSource::new(coder_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                "reducer" => Arc::new(ScriptedSource::new(reducer_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                "advisor" => Arc::new(ScriptedSource::new(advisor_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                _ => Arc::new(ScriptedSource::new(Vec::new())) as Arc<dyn ProviderStreamSource>,
            },
        );

    let mut planner = agent_def(
        "planner",
        &["ask_advisor", "read_file"],
        &["submit_planner_outcome"],
    );
    planner.context_recipe = Some("planner".to_owned());
    let mut coder = agent_def(
        "coder",
        &["ask_advisor", "read_file"],
        &["submit_generator_outcome"],
    );
    coder.context_recipe = Some("generator".to_owned());
    let mut reducer = agent_def(
        "reducer",
        &["ask_advisor", "read_file"],
        &["submit_reducer_outcome"],
    );
    reducer.context_recipe = Some("reducer".to_owned());
    let root = agent_def(
        "root",
        &["delegate_workflow", "ask_advisor", "read_file"],
        &["submit_root_outcome"],
    );

    let (state, _dir) = build_test_state(
        Some(factory),
        vec![root, planner, coder, reducer, advisor_agent()],
    )
    .await;
    // The root blocks forever after delegating; run it on a spawned task and observe
    // the delegated workflow drive to Succeeded through the stores (ids are
    // caller-known via Option A). The abort at the end is teardown only.
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    let run = tokio::spawn({
        let state = state.clone();
        let request_id = request_id.clone();
        async move { run_request(&state, &request_id, "delegate please", Some("sb-1"), None).await }
    });

    // The delegated workflow appears, then drives to Succeeded entirely through
    // the real RuntimeAgentRunner + recording port.
    let mut workflow_id = None;
    for _ in 0..200 {
        if let Some(workflow) = state
            .db
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
            .db
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
        .db
        .iteration_store
        .list_for_workflow(&workflow_id)
        .await
        .unwrap();
    let attempts = state
        .db
        .attempt_store
        .list_for_iteration(&iterations[0].id)
        .await
        .unwrap();
    assert_eq!(attempts[0].status(), eos_types::AttemptStatus::Passed);
    assert_eq!(iterations[0].status, eos_types::IterationStatus::Succeeded);
    assert_eq!(
        state
            .db
            .task_store
            .get(&root_task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running,
        "the parent root task must remain running after the delegated workflow succeeds"
    );

    // Teardown only: the aborted future runs no finalizer (AC3.3).
    run.abort();
}

#[derive(Debug)]
struct DelegateThenTerminalRootSource {
    started: std::sync::atomic::AtomicBool,
    asked_advisor: std::sync::atomic::AtomicBool,
    saw_succeeded: Arc<std::sync::atomic::AtomicBool>,
    checks: std::sync::atomic::AtomicUsize,
}

impl DelegateThenTerminalRootSource {
    fn workflow_handle(request: &LlmRequest) -> Option<String> {
        request.messages.iter().find_map(|message| {
            message.content.iter().find_map(|block| {
                let ContentBlock::ToolResult { content, .. } = block else {
                    return None;
                };
                let value: serde_json::Value = serde_json::from_str(content).ok()?;
                Some(value.get("workflow_id")?.as_str()?.to_owned())
            })
        })
    }

    fn saw_workflow_succeeded(request: &LlmRequest) -> bool {
        request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult { content, .. }
                    if content.contains(" is Succeeded."))
            })
        })
    }
}

#[async_trait::async_trait]
impl ProviderStreamSource for DelegateThenTerminalRootSource {
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
        if Self::saw_workflow_succeeded(request) {
            self.saw_succeeded
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let payload = json!({"status": "success", "outcome": "delegated workflow succeeded"});
            if !self
                .asked_advisor
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(stream_of(tool_use_turn(
                    "toolu_root_advise",
                    "ask_advisor",
                    json!({"tool_name": "submit_root_outcome", "tool_payload": payload}),
                )));
            }
            return Ok(stream_of(tool_use_turn(
                "toolu_root_done",
                "submit_root_outcome",
                payload,
            )));
        }

        if let Some(workflow_id) = Self::workflow_handle(request) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let check_no = self
                .checks
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(stream_of(tool_use_turn(
                &format!("toolu_check_{check_no}"),
                "check_workflow_status",
                json!({
                    "workflow_id": workflow_id,
                }),
            )));
        }

        if !self.started.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Ok(stream_of(tool_use_turn(
                "toolu_delegate",
                "delegate_workflow",
                json!({"goal": "do the subwork"}),
            )));
        }

        Ok(stream_of(Vec::new()))
    }
}

// Phase-1 exit proof: one non-injected request delegates, workflow agents finish
// through the real runtime runner, root observes the closed workflow, gets a real
// advisor approval, and submits `submit_root_outcome`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_delegates_waits_and_submits_terminal() {
    let (planner_turns, coder_turns, reducer_turns) = one_step_workflow_turns();
    let advisor_turns = vec![tool_use_turn(
        "toolu_fb",
        "submit_advisor_feedback",
        json!({"verdict": "approve", "summary": "phase-1 e2e path validated; approve"}),
    )];

    let saw_succeeded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw_succeeded_factory = saw_succeeded.clone();
    let factory: ProviderStreamSourceFactory =
        Arc::new(
            move |_request, agent_state| match agent_state.agent_name.as_str() {
                "root" => Arc::new(DelegateThenTerminalRootSource {
                    started: std::sync::atomic::AtomicBool::new(false),
                    asked_advisor: std::sync::atomic::AtomicBool::new(false),
                    saw_succeeded: saw_succeeded_factory.clone(),
                    checks: std::sync::atomic::AtomicUsize::new(0),
                }) as Arc<dyn ProviderStreamSource>,
                "planner" => Arc::new(eos_testkit::ScriptedSource::new(planner_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                "coder" => Arc::new(eos_testkit::ScriptedSource::new(coder_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                "reducer" => Arc::new(eos_testkit::ScriptedSource::new(reducer_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                "advisor" => Arc::new(eos_testkit::ScriptedSource::new(advisor_turns.clone()))
                    as Arc<dyn ProviderStreamSource>,
                _ => Arc::new(eos_testkit::ScriptedSource::new(Vec::new()))
                    as Arc<dyn ProviderStreamSource>,
            },
        );

    let mut planner = agent_def(
        "planner",
        &["ask_advisor", "read_file"],
        &["submit_planner_outcome"],
    );
    planner.context_recipe = Some("planner".to_owned());
    let mut coder = agent_def(
        "coder",
        &["ask_advisor", "read_file"],
        &["submit_generator_outcome"],
    );
    coder.context_recipe = Some("generator".to_owned());
    let mut reducer = agent_def(
        "reducer",
        &["ask_advisor", "read_file"],
        &["submit_reducer_outcome"],
    );
    reducer.context_recipe = Some("reducer".to_owned());
    let mut root = agent_def(
        "root",
        &[
            "delegate_workflow",
            "check_workflow_status",
            "ask_advisor",
            "read_file",
        ],
        &["submit_root_outcome"],
    );
    root.tool_call_limit = std::num::NonZeroU32::new(40).expect("nonzero");

    let (state, _dir) = build_test_state(
        Some(factory),
        vec![root, planner, coder, reducer, advisor_agent()],
    )
    .await;
    let request_id = RequestId::new_v4();
    let root_task_id = root_task_id_for(&request_id);
    run_request(
        &state,
        &request_id,
        "delegate then finish",
        Some("sb-1"),
        None,
    )
    .await
    .unwrap();

    let workflows = state
        .db
        .workflow_store
        .list_for_parent_task(&root_task_id)
        .await
        .unwrap();
    assert!(
        saw_succeeded.load(std::sync::atomic::Ordering::SeqCst),
        "root must observe workflow success through check_workflow_status before submitting"
    );
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].status, WorkflowStatus::Succeeded);

    let task = state
        .db
        .task_store
        .get(&root_task_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Done,
        "root must submit its terminal after delegated workflow success"
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

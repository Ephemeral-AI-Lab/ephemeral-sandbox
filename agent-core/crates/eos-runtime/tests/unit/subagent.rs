use super::*;

// --- Subagent lifecycle (subagent-remediation-PLAN §5): a real child agent runs
// through `run_agent`, its result surfaces as `finished`, and a live
// subagent is drained (not wedged) at the parent's terminal (D1/D2/D3/D9).
mod subagent_lifecycle {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use eos_engine::{EngineError, EngineStream, EventSource, StreamEvent};
    use eos_llm_client::{ContentBlock, LlmRequest};
    use eos_types::RequestId;
    use eos_types::{AgentDefinition, AgentType, RequestStatus, TaskStatus};
    use serde_json::json;

    use super::run_request;
    use crate::entry::root_task_id_for;
    use crate::runtime_services::support::build_test_state;
    use crate::runtime_services::EventSourceFactory;
    use eos_testkit::{agent_def, text_turn, tool_use_turn, ScriptedSource};

    fn stream_of(events: Vec<StreamEvent>) -> EngineStream {
        Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
    }

    fn root_with_subagent() -> AgentDefinition {
        let mut def = agent_def(
            "root",
            &["run_subagent", "read_file", "ask_advisor"],
            &["submit_root_outcome"],
        );
        // Generous budget so the wait loop never trips the no-terminal ceiling.
        def.tool_call_limit = NonZeroU32::new(40).expect("nonzero");
        def
    }

    fn general_subagent() -> AgentDefinition {
        let mut def = agent_def("subagent", &["read_file"], &["submit_subagent_result"]);
        def.agent_type = AgentType::Subagent;
        def
    }

    fn advisor_agent() -> AgentDefinition {
        let mut def = agent_def("advisor", &["read_file"], &["submit_advisor_feedback"]);
        def.agent_type = AgentType::Advisor;
        def
    }

    fn approve_turn() -> Vec<StreamEvent> {
        tool_use_turn(
            "toolu_fb",
            "submit_advisor_feedback",
            json!({"verdict": "approve", "summary": "subagent path validated; approve"}),
        )
    }

    // D9: a *live* subagent (its run blocks forever) must NOT wedge the root
    // terminal. The `submit_root_outcome` prehook cancels and finalizes it, so
    // the root completes; the old deny-if-count>0 path would have failed the root.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_root_outcome_cancels_live_subagent() {
        let payload = json!({"status": "success", "outcome": "done despite a live subagent"});
        let root_turns = vec![
            tool_use_turn(
                "toolu_sub",
                "run_subagent",
                json!({"agent_name": "subagent", "prompt": "investigate forever"}),
            ),
            tool_use_turn(
                "toolu_advise",
                "ask_advisor",
                json!({"tool_name": "submit_root_outcome", "tool_payload": payload.clone()}),
            ),
            tool_use_turn("toolu_root", "submit_root_outcome", payload.clone()),
        ];
        let advisor_turns = vec![approve_turn()];
        let factory: EventSourceFactory = Arc::new(move |_request, agent_state| match agent_state
            .agent_name
            .as_str()
        {
            "subagent" => {
                Arc::new(ScriptedSource::new_blocking(Vec::new())) as Arc<dyn EventSource>
            }
            "advisor" => {
                Arc::new(ScriptedSource::new(advisor_turns.clone())) as Arc<dyn EventSource>
            }
            _ => Arc::new(ScriptedSource::new(root_turns.clone())) as Arc<dyn EventSource>,
        });

        let (state, _dir) = build_test_state(
            Some(factory),
            vec![root_with_subagent(), advisor_agent(), general_subagent()],
        )
        .await;
        let request_id = RequestId::new_v4();
        let root_task_id = root_task_id_for(&request_id);
        run_request(&state, &request_id, "task", Some("sb-1"), None)
            .await
            .unwrap();

        // `run_request` returns only after the `submit_root_outcome` prehook and
        // the post-run background sweep have both run, so the root reaching Done
        // in the store is the observable proof the live subagent was cancelled
        // rather than wedging the terminal (D9). The per-request session runtime
        // is intentionally not re-exposed, so cancellation is observed through
        // persisted task/request rows.
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
            "a live subagent must not wedge the root terminal — the prehook cancels it (D9)"
        );
        let request = state
            .db
            .request_store
            .get(&request_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request.status,
            RequestStatus::Done,
            "the request finishes Done once the root submits despite the live subagent"
        );
    }

    /// A root probe: launch the subagent, wait until the background completion
    /// notification reaches the transcript, then approve + submit.
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
                    matches!(block, ContentBlock::SystemNotification { text }
                        if text.contains("[BACKGROUND COMPLETED]")
                            && text.contains("agent_run_id=")
                            && text.contains("status=completed"))
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
                    json!({"agent_name": "subagent", "prompt": "investigate"}),
                )));
            }
            // Yield so the spawned subagent run can reach its terminal and the
            // heartbeat can enqueue the completion before the next loop-top drain.
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(stream_of(text_turn("waiting for the subagent completion")))
        }
    }

    // D1/D3 end-to-end: a real subagent child runs, calls
    // `submit_subagent_result`, and completion reaches the root as a
    // `[BACKGROUND COMPLETED]` notification — no test-only fake supervisor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_runs_and_reports_finished() {
        let saw_finished = Arc::new(AtomicBool::new(false));
        let saw_finished_factory = saw_finished.clone();
        let subagent_turns = vec![tool_use_turn(
            "toolu_sub_result",
            "submit_subagent_result",
            json!({"summary": "the bug is at foo.rs:10", "findings": ["foo.rs:10"]}),
        )];
        let advisor_turns = vec![approve_turn()];
        let factory: EventSourceFactory =
            Arc::new(
                move |_request, agent_state| match agent_state.agent_name.as_str() {
                    "subagent" => Arc::new(ScriptedSource::new(subagent_turns.clone()))
                        as Arc<dyn EventSource>,
                    "advisor" => {
                        Arc::new(ScriptedSource::new(advisor_turns.clone())) as Arc<dyn EventSource>
                    }
                    _ => Arc::new(FinishProbeSource {
                        started: Arc::new(AtomicBool::new(false)),
                        asked_advisor: Arc::new(AtomicBool::new(false)),
                        saw_finished: saw_finished_factory.clone(),
                    }) as Arc<dyn EventSource>,
                },
            );

        let (state, _dir) = build_test_state(
            Some(factory),
            vec![root_with_subagent(), advisor_agent(), general_subagent()],
        )
        .await;
        let request_id = RequestId::new_v4();
        let root_task_id = root_task_id_for(&request_id);
        run_request(&state, &request_id, "task", Some("sb-1"), None)
            .await
            .unwrap();

        assert!(
            saw_finished.load(Ordering::SeqCst),
            "the child subagent must run and report completion via notification"
        );
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
            "the root completes after the subagent finishes"
        );
    }

    /// Replays scripted root turns, capturing any `run_subagent` rejection text
    /// from the transcript so the test can assert the Rust error message.
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

    // D2: an unknown dispatch is rejected in-band with the Rust message and
    // mints no record, while the root still completes (the rejection is an
    // in-band tool error, not a wedge).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_subagent_is_rejected_in_band() {
        let payload = json!({"status": "success", "outcome": "rejected cleanly"});
        let root_turns = vec![
            tool_use_turn(
                "toolu_sub",
                "run_subagent",
                json!({"agent_name": "subagent", "prompt": "go"}),
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
        let factory: EventSourceFactory = Arc::new(move |_request, agent_state| {
            if agent_state.agent_name == "advisor" {
                Arc::new(ScriptedSource::new(advisor_turns.clone())) as Arc<dyn EventSource>
            } else {
                Arc::new(RejectionProbe {
                    turns: std::sync::Mutex::new(root_turns.clone()),
                    rejection: rejection_probe.clone(),
                }) as Arc<dyn EventSource>
            }
        });

        // No "subagent" agent registered -> run_subagent must reject "not registered".
        let (state, _dir) =
            build_test_state(Some(factory), vec![root_with_subagent(), advisor_agent()]).await;
        let request_id = RequestId::new_v4();
        let root_task_id = root_task_id_for(&request_id);
        run_request(&state, &request_id, "task", Some("sb-1"), None)
            .await
            .unwrap();

        let captured = rejection.lock().unwrap().clone();
        assert_eq!(
            captured.as_deref(),
            Some("run_subagent: agent 'subagent' is not registered."),
            "an unregistered agent is rejected with the Rust error text"
        );
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
            "an in-band rejection does not wedge the root"
        );
    }
}

use super::*;

// --- Slice 1 instance identity (anchor §7): a backgrounded command-session
// completion is pulled by the per-request heartbeat and delivered to the model
// as a `[BACKGROUND COMPLETED]` SystemNotification in a later provider request.
// This goes through the real `run_request` wiring, proving the heartbeat sink
// and the loop's `notifier` are the SAME `EngineNotificationQueue`. If the wiring
// handed the loop a different instance, the model would never see the
// notification and `saw_notification` would stay false.
mod command_session_delivery {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use eos_engine::{EngineError, EngineStream, ProviderStreamSource, StreamEvent};
    use eos_llm_client::{ContentBlock, LlmRequest};
    use eos_sandbox_port::{DaemonOp, SandboxPortError, SandboxTransport};
    use eos_types::{AgentRegistry, JsonObject, RequestId, SandboxId};
    use eos_types::{AgentType, TaskStatus};
    use serde_json::json;

    use super::run_request;
    use crate::entry::root_task_id_for;
    use crate::runtime::support::{FakeGateway, FakeProvisioner};
    use crate::runtime::ProviderStreamSourceFactory;
    use crate::AgentCoreRuntime;
    use crate::RuntimeConfig;
    use eos_testkit::{agent_def, text_turn, tool_use_turn, ScriptedSource};

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
        ) -> Result<JsonObject, SandboxPortError> {
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
                        "agent_run_id": "root",
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
    impl ProviderStreamSource for DeliveryProbeSource {
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
                    return Ok(stream_of(tool_use_turn(
                        "toolu_advise",
                        "ask_advisor",
                        json!({
                            "tool_name": "submit_root_outcome",
                            "tool_payload": {"status": "success", "outcome": "saw background completion"},
                        }),
                    )));
                }
                return Ok(stream_of(tool_use_turn(
                    "toolu_done",
                    "submit_root_outcome",
                    json!({"status": "success", "outcome": "saw background completion"}),
                )));
            }
            if !self.started.swap(true, Ordering::SeqCst) {
                return Ok(stream_of(tool_use_turn(
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
        let mut root = agent_def(
            "root",
            &["exec_command", "read_file", "ask_advisor"],
            &["submit_root_outcome"],
        );
        // Generous budget so the wait-loop never trips the no-terminal ceiling
        // before the (fast) heartbeat delivers.
        root.tool_call_limit = NonZeroU32::new(40).expect("nonzero");
        let mut advisor = agent_def("advisor", &["read_file"], &["submit_advisor_feedback"]);
        advisor.agent_type = AgentType::Advisor;

        let started = Arc::new(AtomicBool::new(false));
        let asked_advisor = Arc::new(AtomicBool::new(false));
        let saw_notification = Arc::new(AtomicBool::new(false));
        let started_factory = started.clone();
        let asked_factory = asked_advisor.clone();
        let saw_factory = saw_notification.clone();
        // The advisor agent runs a real approve turn; the root probe drives the
        // rest. No injected advisor port — the gate reads the transcript.
        let advisor_turn = tool_use_turn(
            "toolu_fb",
            "submit_advisor_feedback",
            json!({"verdict": "approve", "summary": "background completion is real; approve"}),
        );
        let factory: ProviderStreamSourceFactory = Arc::new(move |_request, agent_state| {
            if agent_state.agent_name == "advisor" {
                Arc::new(ScriptedSource::new(vec![advisor_turn.clone()]))
                    as Arc<dyn ProviderStreamSource>
            } else {
                Arc::new(DeliveryProbeSource {
                    started: started_factory.clone(),
                    asked_advisor: asked_factory.clone(),
                    saw_notification: saw_factory.clone(),
                }) as Arc<dyn ProviderStreamSource>
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        let registry: AgentRegistry = vec![root, advisor].into_iter().collect();
        let state = AgentCoreRuntime::builder()
            .database_url(url)
            .tools_root(eos_testkit::test_tools_root())
            .sandbox_gateway(Arc::new(FakeGateway::new(
                Arc::new(CommandCompletionTransport),
                Arc::new(FakeProvisioner::default()),
            )))
            .agent_registry(Arc::new(registry))
            .provider_stream_source_factory(factory)
            .runtime_config(RuntimeConfig::new(20))
            .build()
            .await
            .expect("build state");

        let request_id = RequestId::new_v4();
        let root_task_id = root_task_id_for(&request_id);
        run_request(
            &state,
            &request_id,
            "run a background command",
            Some("sb-test"),
            None,
        )
        .await
        .expect("run request");

        assert!(
            saw_notification.load(Ordering::SeqCst),
            "the backgrounded completion must reach the model as a SystemNotification \
             (heartbeat sink and loop notifier must be the same instance)"
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
            "seeing the notification lets the root submit its terminal"
        );
    }
}

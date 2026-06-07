//! Subagent orchestration: the `BackgroundSupervisorPort` impl on
//! [`BackgroundSupervisorHandle`] — validate, build the child run, drive
//! `run_agent` on a detached task, and settle the record when it
//! finishes. Ports `tools/subagent/run_subagent/run_subagent.py` (driver +
//! validation + terminal forwarding) and `tools/subagent/control.py`
//! (status/result taxonomy + JSON payload).
//!
//! D3 classification keys on terminal **presence**, not `is_error`: a subagent
//! that called its terminal (even with `is_error=true`) settles `Completed` and
//! reports `finished`; only a crash or no-terminal exit settles `Failed`.

use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::{AgentName, AgentType};
use eos_agent_message_records::AgentRunRecordKind;
use eos_llm_client::Message;
use eos_tools::ports::{
    BackgroundInflightReport, BackgroundSupervisorPort, CommandSessionSupervisorPort,
    SpawnedSubagent, StartedSubagent, StartedWorkflowHandle,
};
use eos_tools::{ExecutionMetadata, ToolError, ToolResult, WorkflowControlPort};
use eos_types::{AgentRunId, JsonObject, SubagentSessionId, WorkflowSessionId};
use serde_json::{json, Value};

use super::handle::BackgroundSupervisorHandle;
use super::supervisor::{BackgroundTaskStatus, SubagentRecord};
use crate::{run_agent, AgentRunInput, AgentRunResult};

const RECURSION_MESSAGE: &str = "run_subagent: subagents may not spawn further subagents. \
     This is a hard contract — handle the work directly or submit your findings via the terminal tool.";

/// Port of `explorer_guidance.py::build_explorer_launch_prompt` (verbatim). The
/// run prompt placed *after* the parent's free-text task message.
fn build_explorer_launch_prompt() -> String {
    "# What's in context\n\
     - Parent's user message above\n\
     \n\
     # What to do\n\
     - Investigate the parent's question and return concrete findings.\n\
     \n\
     ## Deliver\n\
     - File paths, line numbers, specific symbols. No vague hand-waves.\n\
     - Missing context the parent will need to act on the findings.\n\
     - Obvious areas you skipped.\n\
     \n\
     ## Submit\n\
     Call `submit_exploration_result`."
        .to_owned()
}

/// Rust `AgentType.value` (`'agent'` / `'subagent'`) for the error text.
const fn agent_type_value(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Agent => "agent",
        AgentType::Subagent => "subagent",
    }
}

/// Map a settled status to its `background_tool.*` diagnostic event type.
const fn terminal_event_type(status: BackgroundTaskStatus) -> &'static str {
    match status {
        BackgroundTaskStatus::Running => "background_tool.started",
        BackgroundTaskStatus::Completed => "background_tool.completed",
        BackgroundTaskStatus::Failed => "background_tool.failed",
        BackgroundTaskStatus::Cancelled => "background_tool.cancelled",
        BackgroundTaskStatus::Delivered => "background_tool.delivered",
    }
}

/// Rust `BackgroundTaskStatus.value` (lowercase) for diagnostics.
const fn status_value(status: BackgroundTaskStatus) -> &'static str {
    match status {
        BackgroundTaskStatus::Running => "running",
        BackgroundTaskStatus::Completed => "completed",
        BackgroundTaskStatus::Failed => "failed",
        BackgroundTaskStatus::Cancelled => "cancelled",
        BackgroundTaskStatus::Delivered => "delivered",
    }
}

/// Emit one `background_tool.*` diagnostic event. These lifecycle rows are
/// reconstructable from supervisor state and terminal results, so the runner
/// must validate correctness from state, not from tracing.
fn trace_background_tool(
    event_type: &str,
    background_task_id: &SubagentSessionId,
    agent_run_id: &AgentRunId,
    status: BackgroundTaskStatus,
    exit_code: Option<i64>,
) {
    tracing::debug!(
        target: "eos_engine::diagnostics",
        event_type,
        background_task_id = background_task_id.as_str(),
        task_kind = "subagent",
        tool_name = "run_subagent",
        agent_run_id = agent_run_id.as_str(),
        status = status_value(status),
        exit_code,
        "background tool lifecycle"
    );
}

/// Classify a finished agent run into a settled `(status, result, exit_code)`
/// — port of `run_subagent.py:231-251`. Terminal present → `Completed` + the
/// terminal verbatim (incl. its `is_error`) + `subagent_terminal_called:true`;
/// crash / no-terminal → `Failed` with the distinct Rust messages +
/// `subagent_terminal_called:false`.
fn classify_run(run: AgentRunResult) -> (BackgroundTaskStatus, ToolResult, i64) {
    match run.terminal_result {
        Some(terminal) => {
            let exit_code = i64::from(terminal.is_error);
            let mut metadata = terminal.metadata.clone();
            metadata.insert("subagent_terminal_called".to_owned(), json!(true));
            let result = ToolResult {
                output: terminal.output,
                is_error: terminal.is_error,
                metadata,
                is_terminal: terminal.is_terminal,
            };
            (BackgroundTaskStatus::Completed, result, exit_code)
        }
        None => {
            let message = match run.error {
                Some(error) => format!("run_subagent: subagent crashed: {error}"),
                None => "run_subagent: subagent exited without calling a terminal tool. \
                         The findings were not delivered."
                    .to_owned(),
            };
            let result = ToolResult::error(message).meta("subagent_terminal_called", json!(false));
            (BackgroundTaskStatus::Failed, result, 1)
        }
    }
}

/// Whether a settled record carries `subagent_terminal_called:true`.
fn terminal_called(record: &SubagentRecord) -> bool {
    record
        .result
        .as_ref()
        .and_then(|result| result.metadata.get("subagent_terminal_called"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Port of `control.py::_subagent_status_and_result` (live-peek cut, so the
/// running/failed message tail is empty).
fn subagent_status_and_result(record: &SubagentRecord) -> (&'static str, String) {
    let metadata = record.result.as_ref().map(|result| &result.metadata);
    if let Some(reason) = metadata
        .and_then(|m| m.get("subagent_termination_reason"))
        .and_then(Value::as_str)
    {
        return ("terminated", format!("[terminated: {reason}] "));
    }
    if metadata
        .and_then(|m| m.get("subagent_cancelled"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return ("cancelled", "[cancelled] ".to_owned());
    }
    let output = || {
        record
            .result
            .as_ref()
            .map(|result| result.output.clone())
            .unwrap_or_default()
    };
    match record.status {
        BackgroundTaskStatus::Running => ("running", String::new()),
        BackgroundTaskStatus::Completed | BackgroundTaskStatus::Delivered
            if terminal_called(record) =>
        {
            ("finished", output())
        }
        BackgroundTaskStatus::Cancelled => ("cancelled", "[cancelled] ".to_owned()),
        _ => ("failed", output()),
    }
}

#[async_trait]
impl BackgroundSupervisorPort for BackgroundSupervisorHandle {
    async fn spawn(
        &self,
        ctx: &ExecutionMetadata,
        agent_name: &str,
        prompt: &str,
    ) -> Result<SpawnedSubagent, ToolError> {
        let registry = &self.handles.agent_registry;

        // D2 validation (run_subagent.py:125-150), before any record is minted.
        // 1. Recursion: a subagent may not spawn a subagent.
        if let Ok(caller) = AgentName::new(ctx.agent_name.as_str()) {
            if registry.get(&caller).map(|def| def.agent_type) == Some(AgentType::Subagent) {
                return Ok(SpawnedSubagent::Rejected(RECURSION_MESSAGE.to_owned()));
            }
        }
        // 2/3. The target exists and is a subagent.
        let not_registered = || format!("run_subagent: agent '{agent_name}' is not registered.");
        let Ok(target) = AgentName::new(agent_name) else {
            return Ok(SpawnedSubagent::Rejected(not_registered()));
        };
        let Some(sub_def) = registry.get(&target) else {
            return Ok(SpawnedSubagent::Rejected(not_registered()));
        };
        if sub_def.agent_type != AgentType::Subagent {
            return Ok(SpawnedSubagent::Rejected(format!(
                "run_subagent: agent '{agent_name}' is not a subagent \
                 (agent_type='{}'); only subagent-typed agents may be dispatched here.",
                agent_type_value(sub_def.agent_type)
            )));
        }
        let sub_def = (**sub_def).clone();

        // Build the child run input by deriving identity facts from the parent
        // metadata. Runtime services are passed explicitly below.
        let caller_agent_run_id = ctx.require_agent_run_id()?.clone();
        let mut tool_input = JsonObject::new();
        tool_input.insert("agent_name".to_owned(), json!(agent_name));
        tool_input.insert("prompt".to_owned(), json!(prompt));

        let child_run_id = AgentRunId::new_v4();
        // §11.3: the subagent owns its OWN ephemeral `AgentRunControl` — its own
        // notifier, foreground executor, background supervisor, and
        // command-completion heartbeat. Because that heartbeat drains to the
        // subagent's own notifier, the subagent **can** own command sessions
        // (their `[BACKGROUND COMPLETED]` reaches the subagent, not the parent).
        // The subagent's terminal `SubagentRecord` still settles on the PARENT
        // supervisor (`self`) so the parent observes the child's completion.
        let subagent_control = self.control_factory().ephemeral(child_run_id.clone());
        let child_background = subagent_control.background();
        let child_background_port: Arc<dyn BackgroundSupervisorPort> =
            Arc::new(child_background.clone());
        let child_command_port: Arc<dyn CommandSessionSupervisorPort> = Arc::new(child_background);
        let mut child_meta = ctx.clone();
        child_meta.agent_name = sub_def.name.as_str().to_owned();
        child_meta.agent_run_id = Some(child_run_id.clone());
        child_meta.conversation = Arc::from(Vec::<Message>::new());
        child_meta.tool_use_id = None;

        let run_input = AgentRunInput {
            agent: sub_def,
            initial_messages: vec![
                Message::from_user_text(prompt),
                Message::from_user_text(build_explorer_launch_prompt()),
            ],
            task_id: None,
            agent_run_id: child_run_id,
            tool_metadata: child_meta,
            attempt_submission: None,
            workflow_control: None,
            background_supervisor: Some(child_background_port),
            command_session_supervisor: Some(child_command_port),
            notifier: subagent_control.notifications(),
            cancellation: subagent_control.cancellation(),
            foreground: subagent_control.foreground(),
            persist_agent_run: false,
            record_kind: AgentRunRecordKind::Subagent {
                parent_agent_run_id: caller_agent_run_id.clone(),
            },
        };

        let inner = self.inner();
        let handles = self.handles.clone();
        let driver_inner = inner.clone();
        let driver_agent_run_id = caller_agent_run_id.clone();

        // Register, spawn the driver, and store its abort handle under one lock so
        // concurrent cancellation can never miss a not-yet-stored handle.
        let task_id = {
            let mut supervisor = inner.lock().await;
            let task_id = supervisor.register_subagent(tool_input, caller_agent_run_id.clone());
            // Emit `started` while still holding the lock, before the driver can
            // run: the driver cannot acquire the lock to settle + emit its terminal
            // event until this block releases, so `started` strictly precedes any
            // terminal emit and the supervisor stays the single, ordered emitter
            // (D8). Mirrors Rust emitting `started` synchronously inside launch().
            trace_background_tool(
                "background_tool.started",
                &task_id,
                &caller_agent_run_id,
                BackgroundTaskStatus::Running,
                None,
            );
            let driver_task_id = task_id.clone();
            let join = tokio::spawn(async move {
                // Hold the subagent's control for the whole run so its
                // command-completion heartbeat stays alive; dropping it when the
                // run settles aborts the heartbeat (RAII, no leaked task).
                let _subagent_control = subagent_control;
                let run = run_agent(&handles, run_input, None).await;
                let (status, result, exit_code) = classify_run(run);
                {
                    let mut supervisor = driver_inner.lock().await;
                    supervisor.settle_subagent(&driver_task_id, status, result);
                    supervisor.forget_handle(&driver_task_id);
                }
                trace_background_tool(
                    terminal_event_type(status),
                    &driver_task_id,
                    &driver_agent_run_id,
                    status,
                    Some(exit_code),
                );
            });
            supervisor.store_handle(task_id.clone(), join.abort_handle());
            task_id
        };

        Ok(SpawnedSubagent::Launched(StartedSubagent {
            subagent_session_id: task_id,
        }))
    }

    async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
        _last_n_messages: u8,
    ) -> Result<ToolResult, ToolError> {
        let supervisor = self.inner();
        let guard = supervisor.lock().await;
        let Some(record) = guard.get_subagent(subagent_session_id) else {
            // E5: a missing session is an in-band error (control.py:116-123).
            return Ok(ToolResult::error(format!(
                "No subagent session found with ID: {}",
                subagent_session_id.as_str()
            )));
        };
        let (status, result_text) = subagent_status_and_result(record);
        let agent_name = record
            .tool_input
            .get("agent_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let payload = json!({
            "subagent_session_id": subagent_session_id.as_str(),
            "status": status,
            "agent_name": agent_name,
            "result": result_text,
        });
        let output = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        let mut metadata = JsonObject::new();
        metadata.insert("subagent_snapshot".to_owned(), payload);
        Ok(ToolResult {
            output,
            is_error: false,
            metadata,
            is_terminal: false,
        })
    }

    async fn cancel(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> Result<ToolResult, ToolError> {
        let (cancelled, agent_run_id) = {
            let supervisor = self.inner();
            let mut guard = supervisor.lock().await;
            let agent_run_id = guard
                .get_subagent(subagent_session_id)
                .map(|record| record.agent_run_id.clone());
            let cancelled = guard.cancel_subagent(subagent_session_id, reason);
            if cancelled {
                guard.take_and_abort_handle(subagent_session_id);
            }
            (cancelled, agent_run_id)
        };
        if !cancelled {
            // E6: unknown / already-settled cancel is an in-band error
            // (control.py:116-123).
            return Ok(ToolResult::error(format!(
                "Could not cancel subagent session {}. It may have already completed \
                 or does not exist.",
                subagent_session_id.as_str()
            )));
        }
        let Some(agent_run_id) = agent_run_id else {
            return Ok(ToolResult::error(format!(
                "Could not cancel subagent session {}. It may have already completed \
                 or does not exist.",
                subagent_session_id.as_str()
            )));
        };
        trace_background_tool(
            "background_tool.cancelled",
            subagent_session_id,
            &agent_run_id,
            BackgroundTaskStatus::Cancelled,
            None,
        );
        let reason_suffix = if reason.is_empty() {
            String::new()
        } else {
            format!(" Reason: {reason}")
        };
        Ok(ToolResult::ok(format!(
            "Subagent session {} cancellation requested.{reason_suffix}",
            subagent_session_id.as_str()
        )))
    }

    async fn inflight_report(&self, agent_run_id: Option<&AgentRunId>) -> BackgroundInflightReport {
        self.inner().lock().await.inflight_report(agent_run_id)
    }

    async fn cancel_subagents_for_agent_run(
        &self,
        agent_run_id: &AgentRunId,
    ) -> BackgroundInflightReport {
        self.inner()
            .lock()
            .await
            .cancel_subagents_for_agent_run(agent_run_id)
    }

    async fn register_workflow(&self, agent_run_id: &AgentRunId, workflow: &StartedWorkflowHandle) {
        self.inner()
            .lock()
            .await
            .register_workflow(agent_run_id, workflow);
    }

    async fn cancel_workflow_record(
        &self,
        workflow_task_id: &WorkflowSessionId,
        reason: &str,
    ) -> bool {
        self.inner()
            .lock()
            .await
            .cancel_workflow_record(workflow_task_id, reason)
    }

    async fn cancel_for_parent_exit(
        &self,
        agent_run_id: Option<&AgentRunId>,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> BackgroundInflightReport {
        BackgroundSupervisorHandle::cancel_for_parent_exit(
            self,
            agent_run_id,
            workflow_control,
            reason,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use eos_agent_def::{AgentDefinition, AgentRegistry, AgentRole};
    use eos_audit::NoopAuditSink;
    use eos_llm_client::{LlmClient, LlmRequest, LlmStream, ProviderError};
    use eos_sandbox_port::SandboxTransport;
    use eos_skills::SkillRegistry;
    use eos_state::{
        AgentRun, AgentRunStore, CoreError, Sealed as StateSealed, TaskId, UtcDateTime,
    };
    use eos_testkit::{agent_def, test_tools_root, FakeTransport};
    use eos_tools::{SandboxToolService, SkillToolService, ToolConfigSet};

    use crate::{
        AgentRunControlFactory, BackgroundSupervisorFactory, EngineRunHandles,
        ForegroundExecutorFactory,
    };

    use super::*;

    #[derive(Debug)]
    struct NoopLlmClient;

    #[async_trait]
    impl LlmClient for NoopLlmClient {
        async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[derive(Debug, Default)]
    struct NoopAgentRunStore;

    impl StateSealed for NoopAgentRunStore {}

    #[async_trait]
    impl AgentRunStore for NoopAgentRunStore {
        async fn create_run(
            &self,
            agent_run_id: &AgentRunId,
            task_id: &TaskId,
            agent_name: &str,
            initial_messages: Option<&[JsonObject]>,
        ) -> Result<AgentRun, CoreError> {
            Ok(AgentRun {
                id: agent_run_id.clone(),
                task_id: task_id.clone(),
                initial_messages: initial_messages.map(<[_]>::to_vec),
                agent_name: agent_name.to_owned(),
                message_history: None,
                terminal_tool_result: None,
                token_count: 0,
                error: None,
                created_at: UtcDateTime::now(),
                finished_at: None,
            })
        }

        async fn finish_run(
            &self,
            _agent_run_id: &AgentRunId,
            _message_history: Option<&[JsonObject]>,
            _terminal_tool_result: Option<&JsonObject>,
            _token_count: i64,
            _error: Option<&str>,
        ) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get(&self, _agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get_for_task(&self, _task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }
    }

    fn subagent_def(name: &str) -> AgentDefinition {
        let mut def = agent_def(
            name,
            AgentRole::Subagent,
            &[],
            &["submit_exploration_result"],
        );
        def.agent_type = AgentType::Subagent;
        def
    }

    fn handles(agents: Vec<AgentDefinition>) -> EngineRunHandles {
        let transport: Arc<dyn SandboxTransport> = Arc::new(FakeTransport);
        EngineRunHandles {
            agent_run_store: Arc::new(NoopAgentRunStore),
            llm_client: Arc::new(NoopLlmClient),
            event_source_factory: None,
            agent_registry: Arc::new(agents.into_iter().collect::<AgentRegistry>()),
            tool_config: Arc::new(
                ToolConfigSet::load_from_dir(&test_tools_root()).expect("tool config"),
            ),
            sandbox_service: SandboxToolService::new(transport),
            root_submission: None,
            skill_service: SkillToolService::new(Arc::new(SkillRegistry::new())),
            tool_registry_extender: None,
            audit: Arc::new(NoopAuditSink),
            message_records: None,
            workspace_root: "/tmp".to_owned(),
        }
    }

    fn test_control_factory(agents: Vec<AgentDefinition>) -> AgentRunControlFactory {
        AgentRunControlFactory::new(
            ForegroundExecutorFactory,
            BackgroundSupervisorFactory::new(
                handles(agents),
                Arc::new(FakeTransport),
                std::time::Duration::from_secs(3600),
            ),
        )
    }

    fn handle_with_agents(agents: Vec<AgentDefinition>) -> BackgroundSupervisorHandle {
        let control_factory = test_control_factory(agents.clone());
        BackgroundSupervisorHandle::new(handles(agents), Arc::new(FakeTransport), control_factory)
    }

    fn metadata_for(agent_name: &str, agent_run_id: &str) -> ExecutionMetadata {
        let mut metadata = eos_testkit::metadata();
        metadata.agent_name = agent_name.to_owned();
        metadata.agent_run_id = Some(agent_run_id.parse().expect("agent run id"));
        metadata
    }

    fn record_with(status: BackgroundTaskStatus, result: Option<ToolResult>) -> SubagentRecord {
        let mut tool_input = JsonObject::new();
        tool_input.insert("agent_name".to_owned(), json!("explorer"));
        SubagentRecord {
            subagent_session_id: "subagent_1".parse().expect("subagent id"),
            tool_input,
            status,
            agent_run_id: "root".parse().expect("agent run id"),
            result,
        }
    }

    fn terminal_called_flag(result: &ToolResult) -> Option<bool> {
        result
            .metadata
            .get("subagent_terminal_called")
            .and_then(Value::as_bool)
    }

    // D3: a terminal that returned is_error=true STILL settles Completed and
    // reports `finished` — classification keys on terminal presence, not is_error.
    #[test]
    fn terminal_with_error_settles_completed_and_finished() {
        let (status, result, exit_code) = classify_run(AgentRunResult {
            terminal_result: Some(ToolResult::error("partial but delivered")),
            error: None,
        });
        assert_eq!(status, BackgroundTaskStatus::Completed);
        assert!(
            result.is_error,
            "the terminal's is_error rides through verbatim"
        );
        assert_eq!(exit_code, 1);
        assert_eq!(terminal_called_flag(&result), Some(true));
        let record = record_with(BackgroundTaskStatus::Completed, Some(result));
        assert_eq!(subagent_status_and_result(&record).0, "finished");
    }

    #[test]
    fn terminal_ok_settles_completed_finished() {
        let (status, result, exit_code) = classify_run(AgentRunResult {
            terminal_result: Some(ToolResult::ok("findings")),
            error: None,
        });
        assert_eq!(status, BackgroundTaskStatus::Completed);
        assert!(!result.is_error);
        assert_eq!(exit_code, 0);
        assert_eq!(terminal_called_flag(&result), Some(true));
        let record = record_with(BackgroundTaskStatus::Completed, Some(result));
        let (kind, text) = subagent_status_and_result(&record);
        assert_eq!(kind, "finished");
        assert_eq!(text, "findings");
    }

    // D3: a crash with no terminal settles Failed + subagent_terminal_called:false.
    #[test]
    fn crash_settles_failed() {
        let (status, result, exit_code) = classify_run(AgentRunResult {
            terminal_result: None,
            error: Some("provider exploded".to_owned()),
        });
        assert_eq!(status, BackgroundTaskStatus::Failed);
        assert_eq!(exit_code, 1);
        assert!(result
            .output
            .contains("subagent crashed: provider exploded"));
        assert_eq!(terminal_called_flag(&result), Some(false));
        assert_eq!(
            subagent_status_and_result(&record_with(BackgroundTaskStatus::Failed, Some(result))).0,
            "failed"
        );
    }

    // D3: exiting without a terminal settles Failed with the distinct message.
    #[test]
    fn no_terminal_settles_failed() {
        let (status, result, _) = classify_run(AgentRunResult {
            terminal_result: None,
            error: None,
        });
        assert_eq!(status, BackgroundTaskStatus::Failed);
        assert!(result.output.contains("without calling a terminal tool"));
        assert_eq!(terminal_called_flag(&result), Some(false));
    }

    // Taxonomy: running has no message tail (live-peek cut); cancelled is bracketed.
    #[test]
    fn taxonomy_running_and_cancelled() {
        assert_eq!(
            subagent_status_and_result(&record_with(BackgroundTaskStatus::Running, None)),
            ("running", String::new())
        );
        let cancelled = record_with(
            BackgroundTaskStatus::Cancelled,
            Some(ToolResult::error("x").meta("subagent_cancelled", json!(true))),
        );
        assert_eq!(subagent_status_and_result(&cancelled).0, "cancelled");
    }

    // A Completed record WITHOUT subagent_terminal_called falls through to failed
    // (matches control.py's `COMPLETED && terminal_called` guard).
    #[test]
    fn completed_without_terminal_called_is_failed() {
        let record = record_with(BackgroundTaskStatus::Completed, Some(ToolResult::ok("x")));
        assert_eq!(subagent_status_and_result(&record).0, "failed");
    }

    #[test]
    fn explorer_launch_prompt_is_verbatim() {
        let prompt = build_explorer_launch_prompt();
        assert!(prompt.starts_with("# What's in context\n- Parent's user message above"));
        assert!(prompt.ends_with("## Submit\nCall `submit_exploration_result`."));
        assert!(prompt.contains("Investigate the parent's question and return concrete findings."));
    }

    #[tokio::test]
    async fn spawn_rejects_recursion_unknown_and_non_subagent_targets() {
        let root = agent_def("root", AgentRole::Root, &[], &["submit_root_outcome"]);
        let worker = agent_def(
            "worker",
            AgentRole::Generator,
            &[],
            &["submit_generator_outcome"],
        );
        let explorer = subagent_def("explorer");
        let handle = handle_with_agents(vec![root, worker, explorer]);

        let recursive = handle
            .spawn(&metadata_for("explorer", "run-sub"), "explorer", "inspect")
            .await
            .expect("spawn");
        assert!(matches!(
            recursive,
            SpawnedSubagent::Rejected(message) if message.contains("may not spawn further subagents")
        ));

        let missing = handle
            .spawn(&metadata_for("root", "run-root"), "missing", "inspect")
            .await
            .expect("spawn");
        assert!(matches!(
            missing,
            SpawnedSubagent::Rejected(message) if message.contains("is not registered")
        ));

        let non_subagent = handle
            .spawn(&metadata_for("root", "run-root"), "worker", "inspect")
            .await
            .expect("spawn");
        assert!(matches!(
            non_subagent,
            SpawnedSubagent::Rejected(message) if message.contains("is not a subagent")
        ));
    }

    #[tokio::test]
    async fn progress_and_cancel_return_model_facing_results() {
        let handle = handle_with_agents(Vec::new());
        let agent_run_id: AgentRunId = "run-root".parse().expect("agent run id");
        let session_id = {
            let inner = handle.inner();
            let mut guard = inner.lock().await;
            let mut tool_input = JsonObject::new();
            tool_input.insert("agent_name".to_owned(), json!("explorer"));
            guard.register_subagent(tool_input, agent_run_id)
        };

        let running = handle.progress(&session_id, 10).await.expect("progress");
        assert!(!running.is_error);
        let snapshot = running.metadata.get("subagent_snapshot").expect("snapshot");
        assert_eq!(snapshot["status"], json!("running"));
        assert_eq!(snapshot["agent_name"], json!("explorer"));

        let cancelled = handle
            .cancel(&session_id, "no longer needed")
            .await
            .expect("cancel");
        assert!(!cancelled.is_error);
        assert!(cancelled.output.contains("no longer needed"));

        let after_cancel = handle.progress(&session_id, 10).await.expect("progress");
        assert_eq!(
            after_cancel.metadata["subagent_snapshot"]["status"],
            json!("cancelled")
        );

        let unknown: SubagentSessionId = "subagent_missing".parse().expect("subagent id");
        assert!(
            handle
                .progress(&unknown, 10)
                .await
                .expect("progress")
                .is_error
        );
        assert!(
            handle
                .cancel(&unknown, "cleanup")
                .await
                .expect("cancel")
                .is_error
        );

        assert!(
            handle
                .cancel(&session_id, "again")
                .await
                .expect("cancel")
                .is_error,
            "already-settled sessions reject cancellation"
        );
    }
}

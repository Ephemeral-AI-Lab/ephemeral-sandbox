//! Subagent orchestration: the `BackgroundSupervisorPort` impl on
//! [`BackgroundSupervisorHandle`] — validate, build the child run, drive
//! `run_ephemeral_agent` on a detached task, and settle the record when it
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
use eos_llm_client::Message;
use eos_tools::ports::{
    BackgroundInflightReport, BackgroundSupervisorPort, SpawnedSubagent, StartedSubagent,
    StartedWorkflow,
};
use eos_tools::{ExecutionMetadata, ToolError, ToolResult, WorkflowControlPort};
use eos_types::{AgentRunId, JsonObject, SubagentSessionId, WorkflowSessionId};
use serde_json::{json, Value};

use super::supervisor::{BackgroundSupervisorHandle, BackgroundTaskStatus, SubagentRecord};
use crate::agent_loop::{run_ephemeral_agent, EphemeralRun, EphemeralRunInput};
use crate::notifications::NotificationService;

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

/// Python `AgentType.value` (`'agent'` / `'subagent'`) for the error text.
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

/// Python `BackgroundTaskStatus.value` (lowercase) for diagnostics.
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
    task_id: &str,
    agent_id: &str,
    status: BackgroundTaskStatus,
    exit_code: Option<i64>,
) {
    tracing::debug!(
        target: "eos_engine::diagnostics",
        event_type,
        background_task_id = task_id,
        task_kind = "subagent",
        tool_name = "run_subagent",
        agent_id,
        status = status_value(status),
        exit_code,
        "background tool lifecycle"
    );
}

/// Classify a finished ephemeral run into a settled `(status, result, exit_code)`
/// — port of `run_subagent.py:231-251`. Terminal present → `Completed` + the
/// terminal verbatim (incl. its `is_error`) + `subagent_terminal_called:true`;
/// crash / no-terminal → `Failed` with the distinct Python messages +
/// `subagent_terminal_called:false`.
fn classify_run(run: EphemeralRun) -> (BackgroundTaskStatus, ToolResult, i64) {
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

        // Build the child run input by deriving from the parent metadata (the
        // engine has no `&AppState`; the child needs the parent's transport /
        // stores / skill_registry to run its tools).
        let caller_agent_id = ctx.agent_id();
        let mut tool_input = JsonObject::new();
        tool_input.insert("agent_name".to_owned(), json!(agent_name));
        tool_input.insert("prompt".to_owned(), json!(prompt));

        let child_run_id = AgentRunId::new_v4();
        let child_notifier = NotificationService::new();
        let mut child_meta = ctx.clone();
        child_meta.agent_name = sub_def.name.as_str().to_owned();
        child_meta.agent_run_id = Some(child_run_id.clone());
        child_meta.notifications = Some(Arc::new(child_notifier.clone()));
        child_meta.conversation = Arc::from(Vec::<Message>::new());
        child_meta.tool_use_id = None;
        // A subagent must not register background command sessions: the single
        // per-request heartbeat drains only to the root sink, so a subagent's
        // `[BACKGROUND COMPLETED]` would mis-route to the root conversation
        // (anchor §5/D5). Clearing the port makes a subagent's `exec_command`
        // run foreground-only — no supervisor registration, no heartbeat notify.
        child_meta.command_session_supervisor = None;

        let run_input = EphemeralRunInput {
            agent: sub_def,
            initial_messages: vec![
                Message::from_user_text(prompt),
                Message::from_user_text(build_explorer_launch_prompt()),
            ],
            task_id: None,
            agent_run_id: child_run_id,
            tool_metadata: child_meta,
            notifier: child_notifier,
            persist_agent_run: false,
        };

        let inner = self.inner();
        let handles = self.handles.clone();
        let driver_inner = inner.clone();
        let driver_agent_id = caller_agent_id.clone();

        // Register, spawn the driver, and store its abort handle under one lock so
        // concurrent cancellation can never miss a not-yet-stored handle.
        let task_id = {
            let mut supervisor = inner.lock().await;
            let task_id = supervisor.register_subagent(tool_input, Some(caller_agent_id.clone()));
            // Emit `started` while still holding the lock, before the driver can
            // run: the driver cannot acquire the lock to settle + emit its terminal
            // event until this block releases, so `started` strictly precedes any
            // terminal emit and the supervisor stays the single, ordered emitter
            // (D8). Mirrors Python emitting `started` synchronously inside launch().
            trace_background_tool(
                "background_tool.started",
                task_id.as_str(),
                &caller_agent_id,
                BackgroundTaskStatus::Running,
                None,
            );
            let driver_task_id = task_id.clone();
            let join = tokio::spawn(async move {
                let run = run_ephemeral_agent(&handles, run_input, None).await;
                let (status, result, exit_code) = classify_run(run);
                {
                    let mut supervisor = driver_inner.lock().await;
                    supervisor.settle_subagent(&driver_task_id, status, result);
                    supervisor.forget_handle(&driver_task_id);
                }
                trace_background_tool(
                    terminal_event_type(status),
                    driver_task_id.as_str(),
                    &driver_agent_id,
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
        let (cancelled, agent_id) = {
            let supervisor = self.inner();
            let mut guard = supervisor.lock().await;
            let agent_id = guard
                .get_subagent(subagent_session_id)
                .and_then(|record| record.agent_id.clone())
                .unwrap_or_default();
            let cancelled = guard.cancel_subagent(subagent_session_id, reason);
            if cancelled {
                guard.take_and_abort_handle(subagent_session_id);
            }
            (cancelled, agent_id)
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
        trace_background_tool(
            "background_tool.cancelled",
            subagent_session_id.as_str(),
            &agent_id,
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

    async fn inflight_report(&self, agent_id: &str) -> BackgroundInflightReport {
        self.inner().lock().await.inflight_report(agent_id)
    }

    async fn cancel_subagents_for_agent(&self, agent_id: &str) -> BackgroundInflightReport {
        self.inner()
            .lock()
            .await
            .cancel_subagents_for_agent(agent_id)
    }

    async fn register_workflow(&self, agent_id: &str, workflow: &StartedWorkflow) {
        self.inner()
            .lock()
            .await
            .register_workflow(agent_id, workflow);
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
        agent_id: &str,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> BackgroundInflightReport {
        BackgroundSupervisorHandle::cancel_for_parent_exit(self, agent_id, workflow_control, reason)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with(status: BackgroundTaskStatus, result: Option<ToolResult>) -> SubagentRecord {
        let mut tool_input = JsonObject::new();
        tool_input.insert("agent_name".to_owned(), json!("explorer"));
        SubagentRecord {
            subagent_session_id: "subagent_1".parse().expect("subagent id"),
            tool_input,
            status,
            agent_id: Some("root".to_owned()),
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
        let (status, result, exit_code) = classify_run(EphemeralRun {
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
        let (status, result, exit_code) = classify_run(EphemeralRun {
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
        let (status, result, exit_code) = classify_run(EphemeralRun {
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
        let (status, result, _) = classify_run(EphemeralRun {
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
}

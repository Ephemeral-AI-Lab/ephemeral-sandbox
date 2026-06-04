//! `RequireNoBackgroundSessions` is the agent-facing isolated/terminal guard for
//! every background lane: outstanding delegated workflows, in-flight subagents,
//! and daemon-visible command sessions.
//!
//! Terminal tools and `exit_isolated_workspace` settle in-process subagent
//! records before checking the remaining lanes. `enter_isolated_workspace`
//! remains inspect-only. Workflows stay owned by persisted workflow state via
//! [`WorkflowControlPort::find_outstanding`], and command sessions stay owned by
//! the sandbox daemon via `api.v1.command_session_count`.

use eos_types::JsonObject;
use serde_json::{json, Value};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;

use super::{HookDenial, HookOutcome};

/// Whether this protected tool **cancels** the agent's in-flight subagents (the
/// four terminals + `exit_isolated_workspace`) vs only inspects them
/// (`enter_isolated_workspace`, which keeps reject semantics).
fn cancels_inflight_subagents(tool: ToolName) -> bool {
    matches!(
        tool,
        ToolName::SubmitRootOutcome
            | ToolName::SubmitGeneratorOutcome
            | ToolName::SubmitReducerOutcome
            | ToolName::SubmitPlannerOutcome
            | ToolName::ExitIsolatedWorkspace
    )
}

/// Whether this submission is a "bailout" that fails-open on a daemon error
/// (Python `_is_bailout_submission`).
fn is_bailout_submission(tool: ToolName, raw_input: &JsonObject) -> bool {
    match tool {
        ToolName::SubmitPlannerOutcome => raw_input
            .get("deferred_goal_for_next_iteration")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty()),
        ToolName::SubmitGeneratorOutcome | ToolName::SubmitReducerOutcome => raw_input
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| s == "failed"),
        _ => false,
    }
}

fn subagent_in_flight_message(count: usize, tool: ToolName) -> String {
    format!(
        "BLOCKED: {count} subagent background task(s) are still in flight for this agent. \
         Wait for them to finish or cancel them before calling {}, then retry.",
        tool.as_str()
    )
}

fn command_session_in_flight_message(count: usize, tool: ToolName) -> String {
    format!(
        "BLOCKED: {count} command session background task(s) are still in flight for this agent. \
         Finish or interrupt active command sessions before calling {}, then retry.",
        tool.as_str()
    )
}

/// `RequireNoBackgroundSessions`: cancel subagents (or, for `enter_isolated_workspace`,
/// inspect) the agent's in-flight subagents, deny on outstanding workflows, then
/// deny on daemon command sessions (fail-OPEN only for bailout submissions on a
/// daemon error). Invariant: no workflows, no subagents, and no command sessions.
pub(crate) async fn run_require_no_background_sessions(
    tool: ToolName,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
) -> Result<HookOutcome, ToolError> {
    let agent_id = ctx.agent_id();

    if let Some(supervisor) = &ctx.background_supervisor {
        // Terminal/exit tools settle the agent's subagents to 0; enter_isolated
        // only inspects (reject). After cancellation `report.subagent == 0`, so
        // the deny below fires only on the reject path.
        let report = if cancels_inflight_subagents(tool) {
            supervisor.cancel_subagents_for_agent(&agent_id).await
        } else {
            supervisor.inflight_report(&agent_id).await
        };
        if report.subagent > 0 {
            return Ok(HookOutcome::Deny(
                HookDenial::new(
                    subagent_in_flight_message(report.subagent, tool),
                    "no_background_sessions",
                )
                .with_reason("ephemeral_jobs_in_flight")
                .with_count(report.subagent),
            ));
        }
    }

    // Workflow dimension: the supervisor tracks workflow handles, but persisted
    // workflow lifecycle remains authoritative here. Deny while a delegated
    // workflow is still open.
    if let (Some(control), Some(task_id)) = (&ctx.workflow_control, &ctx.task_id) {
        let outstanding = control.find_outstanding(task_id, &agent_id).await?;
        if !outstanding.is_empty() {
            return Ok(HookOutcome::Deny(
                HookDenial::new(
                    format!(
                        "BLOCKED: {} delegated workflow(s) are still outstanding for this agent. \
                         Use check_workflow_status to collect them or cancel_workflow to stop them \
                         before calling {}, then retry.",
                        outstanding.len(),
                        tool.as_str()
                    ),
                    "no_background_sessions",
                )
                .with_reason("ephemeral_jobs_in_flight")
                .with_count(outstanding.len()),
            ));
        }
    }

    let sandbox_id = match &ctx.sandbox_id {
        Some(id) => id,
        None => return Ok(HookOutcome::pass()),
    };

    let daemon = match eos_sandbox_api::command_session_count(
        &*ctx.transport,
        sandbox_id,
        &agent_id,
    )
    .await
    {
        Ok(count) => count as usize,
        Err(_) => {
            if is_bailout_submission(tool, raw_input) {
                // Fail-OPEN: stamp the override reason in the pass-phase
                // metadata so the audit trail distinguishes a bailout from a
                // normal pass (Python `daemon_unavailable_bailout`).
                let mut meta = JsonObject::new();
                meta.insert("policy".to_owned(), json!("no_background_sessions"));
                meta.insert("reason".to_owned(), json!("daemon_unavailable_bailout"));
                return Ok(HookOutcome::Pass(meta));
            }
            return Ok(HookOutcome::Deny(
                    HookDenial::new(
                        format!(
                            "BLOCKED: could not confirm background-task state from the sandbox daemon, \
                             so {} is refused to avoid orphaning in-flight work. Retry shortly.",
                            tool.as_str()
                        ),
                        "no_background_sessions",
                    )
                    .with_reason("command_session_count_unavailable"),
                ));
        }
    };
    if daemon > 0 {
        return Ok(HookOutcome::Deny(
            HookDenial::new(
                command_session_in_flight_message(daemon, tool),
                "no_background_sessions",
            )
            .with_reason("ephemeral_jobs_in_flight")
            .with_count(daemon),
        ));
    }
    Ok(HookOutcome::pass())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use async_trait::async_trait;
    use eos_types::{SubagentSessionId, TaskId, WorkflowId, WorkflowSessionId};

    use crate::ports::{
        BackgroundInflightReport, BackgroundSupervisorPort, OutstandingWorkflow, Sealed,
        SpawnedSubagent, StartedWorkflow, WorkflowControlPort,
    };
    use crate::ToolResult;

    struct ReportSupervisor {
        report: BackgroundInflightReport,
        cancel_called: AtomicBool,
    }

    impl ReportSupervisor {
        const fn new(report: BackgroundInflightReport) -> Self {
            Self {
                report,
                cancel_called: AtomicBool::new(false),
            }
        }
    }

    impl Sealed for ReportSupervisor {}

    #[async_trait]
    impl BackgroundSupervisorPort for ReportSupervisor {
        async fn spawn(
            &self,
            _: &ExecutionMetadata,
            _: &str,
            _: &str,
        ) -> Result<SpawnedSubagent, ToolError> {
            unreachable!("not used by hook tests")
        }

        async fn progress(&self, _: &SubagentSessionId, _: u8) -> Result<ToolResult, ToolError> {
            unreachable!("not used by hook tests")
        }

        async fn cancel(&self, _: &SubagentSessionId, _: &str) -> Result<ToolResult, ToolError> {
            unreachable!("not used by hook tests")
        }

        async fn inflight_report(&self, _: &str) -> BackgroundInflightReport {
            self.report
        }

        async fn cancel_subagents_for_agent(&self, _: &str) -> BackgroundInflightReport {
            self.cancel_called.store(true, Ordering::Relaxed);
            BackgroundInflightReport {
                subagent: 0,
                total: self.report.workflow + self.report.command_session,
                ..self.report
            }
        }

        async fn register_workflow(&self, _: &str, _: &StartedWorkflow) {}

        async fn cancel_workflow_record(&self, _: &WorkflowSessionId, _: &str) -> bool {
            false
        }

        async fn cancel_for_parent_exit(
            &self,
            _: &str,
            _: Option<Arc<dyn WorkflowControlPort>>,
            _: &str,
        ) -> BackgroundInflightReport {
            unreachable!("not used by hook tests")
        }
    }

    struct OneOutstanding;
    impl Sealed for OneOutstanding {}

    #[async_trait]
    impl WorkflowControlPort for OneOutstanding {
        async fn start(&self, _: &TaskId, _: &str, _: &str) -> Result<StartedWorkflow, ToolError> {
            unreachable!("deny short-circuits before start")
        }

        async fn status(
            &self,
            _: &WorkflowId,
            _: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            unreachable!()
        }

        async fn cancel(&self, _: &WorkflowSessionId, _: &str) -> Result<String, ToolError> {
            unreachable!()
        }

        async fn find_outstanding(
            &self,
            _: &TaskId,
            _: &str,
        ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
            Ok(vec![OutstandingWorkflow {
                workflow_id: WorkflowId::new_v4(),
                workflow_task_id: WorkflowSessionId::new_v4(),
                workflow_goal: "prior goal".to_owned(),
            }])
        }

        async fn workflow_depth(&self, _: &WorkflowId) -> Result<u32, ToolError> {
            Ok(1)
        }
    }

    const fn report(
        subagent: usize,
        workflow: usize,
        command_session: usize,
    ) -> BackgroundInflightReport {
        BackgroundInflightReport {
            total: subagent + workflow + command_session,
            subagent,
            workflow,
            command_session,
        }
    }

    fn json_object(value: Value) -> JsonObject {
        match value {
            Value::Object(object) => object,
            _ => JsonObject::new(),
        }
    }

    // A daemon-count failure on a *bailout* submission fails OPEN, and the
    // pass-phase metadata records the override reason (Python parity:
    // `daemon_unavailable_bailout`).
    #[tokio::test]
    async fn bailout_pass_carries_daemon_unavailable_reason() {
        use eos_sandbox_api::SandboxApiError;

        use crate::testsupport::{metadata, FakeTransport};

        let mut ctx = metadata();
        ctx.sandbox_id = Some("sandbox-1".parse().expect("sandbox id"));
        // Every daemon RPC (here, command_session_count) errors.
        ctx.transport = Arc::new(FakeTransport::new(|_, _| {
            Err(SandboxApiError::Transport {
                code: None,
                message: "daemon down".to_owned(),
            })
        }));

        // A failed generator submission qualifies as a bailout.
        let mut input = JsonObject::new();
        input.insert("status".to_owned(), Value::String("failed".to_owned()));

        let outcome =
            run_require_no_background_sessions(ToolName::SubmitGeneratorOutcome, &input, &ctx)
                .await
                .expect("hook ran");
        match outcome {
            HookOutcome::Pass(meta) => {
                assert_eq!(meta["reason"], json!("daemon_unavailable_bailout"));
                assert_eq!(meta["policy"], json!("no_background_sessions"));
            }
            other => panic!("expected a bailout pass, got {other:?}"),
        }
    }

    // The workflow dimension: a terminal is denied while a delegated workflow is
    // still outstanding, gated on the authoritative WorkflowControlPort rather
    // than a supervisor handle record.
    #[tokio::test]
    async fn outstanding_workflow_denies_terminal() {
        use crate::testsupport::metadata;

        let mut ctx = metadata();
        ctx.task_id = Some("parent".parse().expect("task id"));
        ctx.workflow_control = Some(Arc::new(OneOutstanding));

        let outcome = run_require_no_background_sessions(
            ToolName::SubmitRootOutcome,
            &JsonObject::new(),
            &ctx,
        )
        .await
        .expect("hook ran");
        match outcome {
            HookOutcome::Deny(denial) => {
                assert!(
                    denial.message.contains("delegated workflow"),
                    "{}",
                    denial.message
                );
                assert_eq!(denial.extra.get("count").and_then(Value::as_u64), Some(1));
            }
            other => panic!("expected a workflow deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enter_isolated_workspace_denies_inflight_subagents_without_cancelling() {
        use crate::testsupport::metadata;

        let supervisor = Arc::new(ReportSupervisor::new(report(1, 0, 0)));
        let mut ctx = metadata();
        ctx.background_supervisor = Some(supervisor.clone());

        let outcome = run_require_no_background_sessions(
            ToolName::EnterIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
        )
        .await
        .expect("hook ran");

        match outcome {
            HookOutcome::Deny(denial) => {
                assert!(denial.message.contains("subagent"), "{}", denial.message);
                assert_eq!(denial.reason.as_deref(), Some("ephemeral_jobs_in_flight"));
                assert_eq!(denial.extra.get("count").and_then(Value::as_u64), Some(1));
            }
            other => panic!("expected a subagent deny, got {other:?}"),
        }
        assert!(
            !supervisor.cancel_called.load(Ordering::Relaxed),
            "enter must inspect, not cancel, subagents"
        );
    }

    #[tokio::test]
    async fn enter_isolated_workspace_denies_outstanding_workflows() {
        use crate::testsupport::metadata;

        let mut ctx = metadata();
        ctx.task_id = Some("parent".parse().expect("task id"));
        ctx.workflow_control = Some(Arc::new(OneOutstanding));

        let outcome = run_require_no_background_sessions(
            ToolName::EnterIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
        )
        .await
        .expect("hook ran");

        match outcome {
            HookOutcome::Deny(denial) => {
                assert!(
                    denial.message.contains("delegated workflow"),
                    "{}",
                    denial.message
                );
                assert_eq!(denial.reason.as_deref(), Some("ephemeral_jobs_in_flight"));
                assert_eq!(denial.extra.get("count").and_then(Value::as_u64), Some(1));
            }
            other => panic!("expected a workflow deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enter_isolated_workspace_denies_daemon_command_sessions() {
        use eos_sandbox_api::DaemonOp;

        use crate::testsupport::{metadata, FakeTransport};

        let mut ctx = metadata();
        ctx.sandbox_id = Some("sandbox-1".parse().expect("sandbox id"));
        ctx.transport = Arc::new(FakeTransport::new(|op, _| match op {
            DaemonOp::CommandSessionCount => Ok(json_object(json!({"count": 2}))),
            _ => Ok(JsonObject::new()),
        }));

        let outcome = run_require_no_background_sessions(
            ToolName::EnterIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
        )
        .await
        .expect("hook ran");

        match outcome {
            HookOutcome::Deny(denial) => {
                assert!(
                    denial.message.contains("command session"),
                    "{}",
                    denial.message
                );
                assert_eq!(denial.reason.as_deref(), Some("ephemeral_jobs_in_flight"));
                assert_eq!(denial.extra.get("count").and_then(Value::as_u64), Some(2));
            }
            other => panic!("expected a command-session deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_isolated_workspace_cancels_subagents_then_denies_command_sessions() {
        use eos_sandbox_api::DaemonOp;

        use crate::testsupport::{metadata, FakeTransport};

        let supervisor = Arc::new(ReportSupervisor::new(report(3, 0, 0)));
        let mut ctx = metadata();
        ctx.background_supervisor = Some(supervisor.clone());
        ctx.sandbox_id = Some("sandbox-1".parse().expect("sandbox id"));
        ctx.transport = Arc::new(FakeTransport::new(|op, _| match op {
            DaemonOp::CommandSessionCount => Ok(json_object(json!({"count": 1}))),
            _ => Ok(JsonObject::new()),
        }));

        let outcome = run_require_no_background_sessions(
            ToolName::ExitIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
        )
        .await
        .expect("hook ran");

        assert!(
            supervisor.cancel_called.load(Ordering::Relaxed),
            "exit should settle subagent records before checking daemon sessions"
        );
        match outcome {
            HookOutcome::Deny(denial) => {
                assert!(
                    denial.message.contains("command session"),
                    "{}",
                    denial.message
                );
                assert_eq!(denial.reason.as_deref(), Some("ephemeral_jobs_in_flight"));
                assert_eq!(denial.extra.get("count").and_then(Value::as_u64), Some(1));
            }
            other => panic!("expected a command-session deny, got {other:?}"),
        }
    }
}

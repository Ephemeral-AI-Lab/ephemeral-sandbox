//! `RequireNoBackgroundSessions` is the agent-facing isolated/terminal guard for
//! every background session family: outstanding delegated workflows, in-flight
//! subagents, and daemon-visible command sessions.
//!
//! Terminal tools and `exit_isolated_workspace` settle in-process subagent
//! records before checking the remaining session families. `enter_isolated_workspace`
//! remains inspect-only. Workflows stay owned by persisted workflow state via
//! [`WorkflowControlPort::find_outstanding`], and command sessions stay owned by
//! the sandbox daemon via `api.v1.command_session_count`.

use eos_types::JsonObject;
use serde_json::{json, Value};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::tools::HookServices;

use super::{deferred_goal, HookDenial, HookOutcome};

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
/// (Rust `_is_bailout_submission`).
fn is_bailout_submission(tool: ToolName, raw_input: &JsonObject) -> bool {
    match tool {
        ToolName::SubmitPlannerOutcome => deferred_goal(raw_input).is_some(),
        ToolName::SubmitGeneratorOutcome | ToolName::SubmitReducerOutcome => raw_input
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| s == "failed"),
        _ => false,
    }
}

fn subagent_in_flight_message(count: usize, tool: ToolName) -> String {
    format!(
        "BLOCKED: {count} subagent background task(s) are still in flight for this agent run. \
         Wait for them to finish or cancel them before calling {}, then retry.",
        tool.as_str()
    )
}

fn command_session_in_flight_message(count: usize, tool: ToolName) -> String {
    format!(
        "BLOCKED: {count} command session background task(s) are still in flight for this agent run. \
         Finish or interrupt active command sessions before calling {}, then retry.",
        tool.as_str()
    )
}

/// `RequireNoBackgroundSessions`: cancel subagents (or, for `enter_isolated_workspace`,
/// inspect) the agent run's in-flight subagents, deny on outstanding workflows, then
/// deny on daemon command sessions (fail-OPEN only for bailout submissions on a
/// daemon error). Invariant: no workflows, no subagents, and no command sessions.
pub(crate) async fn run_require_no_background_sessions(
    tool: ToolName,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
    services: &HookServices,
) -> Result<HookOutcome, ToolError> {
    let agent_run_id = ctx.require_agent_run_id()?;

    if let Some(supervisor) = &services.background_supervisor {
        // Terminal/exit tools settle the agent's subagents to 0; enter_isolated
        // only inspects (reject). After cancellation `report.subagent == 0`, so
        // the deny below fires only on the reject path.
        let report = if cancels_inflight_subagents(tool) {
            supervisor.cancel_subagents().await
        } else {
            supervisor.running_background_tasks().await
        };
        if report.subagents > 0 {
            return Ok(HookOutcome::Deny(
                HookDenial::new(
                    subagent_in_flight_message(report.subagents, tool),
                    "no_background_sessions",
                )
                .with_reason("ephemeral_jobs_in_flight")
                .with_count(report.subagents),
            ));
        }
    }

    // Workflow dimension: the supervisor tracks workflow handles, but persisted
    // workflow lifecycle remains authoritative here. Deny while a delegated
    // workflow is still open.
    if let (Some(control), Some(task_id)) = (&services.workflow_control, &ctx.task_id) {
        let outstanding = control.find_outstanding(task_id, agent_run_id).await?;
        if !outstanding.is_empty() {
            return Ok(HookOutcome::Deny(
                HookDenial::new(
                    format!(
                        "BLOCKED: {} delegated workflow(s) are still outstanding for this agent run. \
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

    let Some(transport) = &services.sandbox_transport else {
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
    };

    let daemon = match eos_sandbox_port::command_session_count(
        &**transport,
        sandbox_id,
        agent_run_id.as_str(),
    )
    .await
    {
        Ok(count) => count as usize,
        Err(_) => {
            if is_bailout_submission(tool, raw_input) {
                // Fail-OPEN: stamp the override reason in the pass-phase
                // metadata so the audit trail distinguishes a bailout from a
                // normal pass (Rust `daemon_unavailable_bailout`).
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
    use eos_types::{AgentRunId, SubagentSessionId, TaskId, WorkflowId, WorkflowSessionId};

    use crate::ports::{
        BackgroundSupervisorPort, CancelledSubagent, OutstandingWorkflow, RunningBackgroundTasks,
        Sealed, SpawnedSubagent, StartedWorkflowHandle, SubagentLaunch, SubagentProgress,
        WorkflowControlPort,
    };
    struct ReportSupervisor {
        report: RunningBackgroundTasks,
        cancel_called: AtomicBool,
    }

    impl ReportSupervisor {
        const fn new(report: RunningBackgroundTasks) -> Self {
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
            _: SubagentLaunch,
        ) -> Result<SpawnedSubagent, ToolError> {
            unreachable!("not used by hook tests")
        }

        async fn progress(
            &self,
            _: &SubagentSessionId,
            _: u8,
        ) -> Result<SubagentProgress, ToolError> {
            unreachable!("not used by hook tests")
        }

        async fn cancel(
            &self,
            _: &SubagentSessionId,
            _: &str,
        ) -> Result<CancelledSubagent, ToolError> {
            unreachable!("not used by hook tests")
        }

        async fn running_background_tasks(&self) -> RunningBackgroundTasks {
            self.report
        }

        async fn cancel_subagents(&self) -> RunningBackgroundTasks {
            self.cancel_called.store(true, Ordering::Relaxed);
            RunningBackgroundTasks {
                subagents: 0,
                total: self.report.workflows + self.report.command_sessions,
                ..self.report
            }
        }

        async fn register_workflow(&self, _: &StartedWorkflowHandle) {}

        async fn cancel_workflow_record(&self, _: &WorkflowSessionId, _: &str) -> bool {
            false
        }

        async fn teardown(
            &self,
            _: Option<Arc<dyn WorkflowControlPort>>,
            _: &str,
        ) -> RunningBackgroundTasks {
            unreachable!("not used by hook tests")
        }
    }

    struct OneOutstanding;
    impl Sealed for OneOutstanding {}

    #[async_trait]
    impl WorkflowControlPort for OneOutstanding {
        async fn start(
            &self,
            _: &TaskId,
            _: &AgentRunId,
            _: &str,
        ) -> Result<StartedWorkflowHandle, ToolError> {
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
            _: &AgentRunId,
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
        subagents: usize,
        workflows: usize,
        command_sessions: usize,
    ) -> RunningBackgroundTasks {
        RunningBackgroundTasks {
            total: subagents + workflows + command_sessions,
            subagents,
            workflows,
            command_sessions,
        }
    }

    fn json_object(value: Value) -> JsonObject {
        match value {
            Value::Object(object) => object,
            _ => JsonObject::new(),
        }
    }

    fn bind_agent_run(ctx: &mut ExecutionMetadata) {
        let agent_run_id: AgentRunId = "agent-run-test".parse().expect("agent run id");
        ctx.agent_run_id = Some(agent_run_id);
    }

    // A daemon-count failure on a *bailout* submission fails OPEN, and the
    // pass-phase metadata records the override reason (Rust parity:
    // `daemon_unavailable_bailout`).
    #[tokio::test]
    async fn bailout_pass_carries_daemon_unavailable_reason() {
        use eos_sandbox_port::SandboxPortError;

        use crate::support::{metadata, FakeTransport};

        let mut ctx = metadata();
        bind_agent_run(&mut ctx);
        ctx.sandbox_id = Some("sandbox-1".parse().expect("sandbox id"));
        // Every daemon RPC (here, command_session_count) errors.
        let services = crate::tools::HookServices::new(
            Some(Arc::new(FakeTransport::new(|_, _| {
                Err(SandboxPortError::Transport {
                    code: None,
                    message: "daemon down".to_owned(),
                })
            }))),
            None,
            None,
        );

        // A failed generator submission qualifies as a bailout.
        let mut input = JsonObject::new();
        input.insert("status".to_owned(), Value::String("failed".to_owned()));

        let outcome = run_require_no_background_sessions(
            ToolName::SubmitGeneratorOutcome,
            &input,
            &ctx,
            &services,
        )
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
        use crate::support::metadata;

        let mut ctx = metadata();
        bind_agent_run(&mut ctx);
        ctx.task_id = Some("parent".parse().expect("task id"));
        let services = crate::tools::HookServices::new(None, Some(Arc::new(OneOutstanding)), None);

        let outcome = run_require_no_background_sessions(
            ToolName::SubmitRootOutcome,
            &JsonObject::new(),
            &ctx,
            &services,
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
        use crate::support::metadata;

        let supervisor = Arc::new(ReportSupervisor::new(report(1, 0, 0)));
        let mut ctx = metadata();
        bind_agent_run(&mut ctx);
        let services = crate::tools::HookServices::new(None, None, Some(supervisor.clone()));

        let outcome = run_require_no_background_sessions(
            ToolName::EnterIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
            &services,
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
        use crate::support::metadata;

        let mut ctx = metadata();
        bind_agent_run(&mut ctx);
        ctx.task_id = Some("parent".parse().expect("task id"));
        let services = crate::tools::HookServices::new(None, Some(Arc::new(OneOutstanding)), None);

        let outcome = run_require_no_background_sessions(
            ToolName::EnterIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
            &services,
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
        use eos_sandbox_port::DaemonOp;

        use crate::support::{metadata, FakeTransport};

        let mut ctx = metadata();
        bind_agent_run(&mut ctx);
        ctx.sandbox_id = Some("sandbox-1".parse().expect("sandbox id"));
        let services = crate::tools::HookServices::new(
            Some(Arc::new(FakeTransport::new(|op, _| match op {
                DaemonOp::CommandSessionCount => Ok(json_object(json!({"count": 2}))),
                _ => Ok(JsonObject::new()),
            }))),
            None,
            None,
        );

        let outcome = run_require_no_background_sessions(
            ToolName::EnterIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
            &services,
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
        use eos_sandbox_port::DaemonOp;

        use crate::support::{metadata, FakeTransport};

        let supervisor = Arc::new(ReportSupervisor::new(report(3, 0, 0)));
        let mut ctx = metadata();
        bind_agent_run(&mut ctx);
        ctx.sandbox_id = Some("sandbox-1".parse().expect("sandbox id"));
        let services = crate::tools::HookServices::new(
            Some(Arc::new(FakeTransport::new(|op, _| match op {
                DaemonOp::CommandSessionCount => Ok(json_object(json!({"count": 1}))),
                _ => Ok(JsonObject::new()),
            }))),
            None,
            Some(supervisor.clone()),
        );

        let outcome = run_require_no_background_sessions(
            ToolName::ExitIsolatedWorkspace,
            &JsonObject::new(),
            &ctx,
            &services,
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

//! The no-inflight-background-tasks prehook — relocated out of the `hooks.rs`
//! monolith into its own file (mirrors `hooks/advisor_approval.rs`), porting
//! Python `tools/_hooks/require_no_inflight_background_tasks.py`.
//!
//! **Behavior change from HEAD's deny-if-count>0 → drain-to-0 (D6/D9).** On a
//! terminal / `exit_isolated_workspace` call this hook **drains** the agent's
//! in-flight subagent runs (settle `Cancelled` + abort) so a live or phantom
//! subagent never permanently wedges the agent's terminal (the D9 active harm).
//! `enter_isolated_workspace` keeps **reject** semantics (it only inspects).
//!
//! **Scope (resolves the plan's flagged §3e open item).** The drain settles only
//! the supervisor's in-process subagent records — the kind whose phantom causes
//! D9. Command sessions stay on the existing **daemon-authoritative** deny +
//! bailout path: in committed reality a command session is a live daemon-backed
//! process (not a cheap in-process record as the plan assumed), so "draining" one
//! means killing a running build at terminal time. That is a behavior change to
//! just-committed code, a divergence from Python's deny-on-live-command-session,
//! and unnecessary to close D9 — so it is deliberately out of scope here.
//!
//! **Workflow dimension (§3e, all-three-kinds invariant).** Delegated workflows
//! are owned by the workflow lane (a sibling crate) with authoritative persisted
//! state, so the supervisor cannot hold workflow records without duplicating that
//! state and risking a never-settled phantom (a D9 re-run for workflows). Instead
//! this hook gates on the authoritative [`WorkflowControlPort::find_outstanding`]
//! and **denies** the terminal while a delegated workflow is open — Python
//! `count_by_agent` parity (workflows are never auto-cancelled; the agent resolves
//! them via `check_workflow_status` / `cancel_workflow`).

use eos_types::JsonObject;
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;

use super::{HookDenial, HookOutcome};

/// Whether this protected tool **drains** the agent's in-flight subagents (the
/// four terminals + `exit_isolated_workspace`) vs only inspects them
/// (`enter_isolated_workspace`, which keeps reject semantics).
fn drains_inflight(tool: ToolName) -> bool {
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

fn in_flight_message(count: usize, tool: ToolName) -> String {
    format!(
        "BLOCKED: {count} sandbox-bound background task(s) are still in flight for this agent. \
         Finish or interrupt active command sessions before calling {}, then retry.",
        tool.as_str()
    )
}

/// `RequireNoInflightBackgroundTasks`: drain (or, for `enter_isolated_workspace`,
/// inspect) the agent's in-flight subagents, then the daemon command-session
/// count (deny; fail-OPEN only for bailout submissions on a daemon error).
pub(crate) async fn run_require_no_inflight(
    tool: ToolName,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
) -> Result<HookOutcome, ToolError> {
    let agent_id = ctx.agent_id();

    if let Some(supervisor) = &ctx.subagent_supervisor {
        // Draining terminals settle the agent's subagents to 0; enter_isolated
        // only inspects (reject). After a drain `report.subagent == 0`, so the
        // deny below fires only on the reject path.
        let report = if drains_inflight(tool) {
            supervisor.drain_for_agent(&agent_id).await
        } else {
            supervisor.inflight_report(&agent_id).await
        };
        if report.subagent > 0 {
            return Ok(HookOutcome::Deny(
                HookDenial::new(
                    in_flight_message(report.subagent, tool),
                    "no_inflight_background_tasks",
                )
                .with_reason("ephemeral_jobs_in_flight")
                .with_count(report.subagent),
            ));
        }
    }

    // Workflow dimension (BackgroundInflightReport.workflow): delegated workflows
    // are owned by the workflow lane (a sibling crate) with authoritative
    // persisted state, so the supervisor cannot track them. Gate on the
    // authoritative `WorkflowControlPort::find_outstanding` instead — Python
    // `count_by_agent` parity: DENY while a delegated workflow is still open
    // (never auto-cancel; the agent collects/cancels it explicitly via
    // check_workflow_status / cancel_workflow).
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
                    "no_inflight_background_tasks",
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
                meta.insert("policy".to_owned(), json!("no_inflight_background_tasks"));
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
                        "no_inflight_background_tasks",
                    )
                    .with_reason("command_session_count_unavailable"),
                ));
        }
    };
    if daemon > 0 {
        return Ok(HookOutcome::Deny(
            HookDenial::new(
                in_flight_message(daemon, tool),
                "no_inflight_background_tasks",
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

    // A daemon-count failure on a *bailout* submission fails OPEN, and the
    // pass-phase metadata records the override reason (Python parity:
    // `daemon_unavailable_bailout`).
    #[tokio::test]
    async fn bailout_pass_carries_daemon_unavailable_reason() {
        use std::sync::Arc;

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

        let outcome = run_require_no_inflight(ToolName::SubmitGeneratorOutcome, &input, &ctx)
            .await
            .expect("hook ran");
        match outcome {
            HookOutcome::Pass(meta) => {
                assert_eq!(meta["reason"], json!("daemon_unavailable_bailout"));
                assert_eq!(meta["policy"], json!("no_inflight_background_tasks"));
            }
            other => panic!("expected a bailout pass, got {other:?}"),
        }
    }

    // The workflow dimension: a terminal is DENIED while a delegated workflow is
    // still outstanding — gated on the authoritative WorkflowControlPort, not a
    // supervisor record (Python `count_by_agent` parity).
    #[tokio::test]
    async fn outstanding_workflow_denies_terminal() {
        use std::sync::Arc;

        use async_trait::async_trait;
        use eos_types::{TaskId, WorkflowId, WorkflowSessionId};

        use crate::ports::{OutstandingWorkflow, Sealed, StartedWorkflow, WorkflowControlPort};
        use crate::testsupport::metadata;

        struct OneOutstanding;
        impl Sealed for OneOutstanding {}

        #[async_trait]
        impl WorkflowControlPort for OneOutstanding {
            async fn start(
                &self,
                _: &TaskId,
                _: &str,
                _: &str,
            ) -> Result<StartedWorkflow, ToolError> {
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
            async fn is_nested_workflow(&self, _: &WorkflowId) -> Result<bool, ToolError> {
                Ok(false)
            }
        }

        let mut ctx = metadata();
        ctx.task_id = Some("parent".parse().expect("task id"));
        ctx.workflow_control = Some(Arc::new(OneOutstanding));

        let outcome = run_require_no_inflight(ToolName::SubmitRootOutcome, &JsonObject::new(), &ctx)
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
}

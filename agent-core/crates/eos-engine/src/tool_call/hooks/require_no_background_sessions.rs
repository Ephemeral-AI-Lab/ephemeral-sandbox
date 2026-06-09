//! `RequireNoBackgroundSessions` is the agent-facing isolated/terminal guard for
//! every background session family: outstanding delegated workflows, in-flight
//! subagents, and daemon-visible command sessions.
//!
//! Terminal tools and `exit_isolated_workspace` settle in-process subagent
//! records before checking the remaining session families. `enter_isolated_workspace`
//! remains inspect-only. The engine background aggregate is the single source for
//! subagent, delegated-workflow, and background command-session counts.

use eos_tool::{ToolError, ToolName};
use eos_types::BackgroundSessionCounts;

use super::{HookDenial, HookOutcome, ToolCallHooks};

/// Whether this protected tool **cancels** the agent's in-flight subagents (the
/// four terminals + `exit_isolated_workspace`) vs only inspects them
/// (`enter_isolated_workspace`, which keeps reject semantics).
fn cancels_inflight_subagents(tool: ToolName) -> bool {
    matches!(
        tool,
        ToolName::SubmitRootTaskOutcome
            | ToolName::SubmitPlanOutcome
            | ToolName::SubmitWorkerOutcome
            | ToolName::ExitIsolatedWorkspace
    )
}

fn background_in_flight_message(counts: BackgroundSessionCounts, tool: ToolName) -> String {
    format!(
        "BLOCKED: {} background task(s) are still in flight for this agent run \
         (subagents={}, workflows={}, command_sessions={}). Finish, collect, or cancel them \
         before calling {}, then retry.",
        counts.total,
        counts.subagents,
        counts.workflows,
        counts.command_sessions,
        tool.as_str()
    )
}

/// `RequireNoBackgroundSessions`: cancel subagents (or, for `enter_isolated_workspace`,
/// inspect) the agent run's in-flight subagents, then deny if any tracked
/// background family remains. Invariant: no workflows, no subagents, and no
/// command sessions.
pub(crate) async fn run_require_no_background_sessions(
    tool: ToolName,
    hooks: &ToolCallHooks,
) -> Result<HookOutcome, ToolError> {
    if cancels_inflight_subagents(tool) {
        hooks
            .cancel_all_subagents("parent submitted its terminal")
            .await?;
    }

    let counts = hooks.background_counts().await?;
    if counts.total > 0 {
        return Ok(HookOutcome::Deny(
            HookDenial::new(
                background_in_flight_message(counts, tool),
                "no_background_sessions",
            )
            .with_reason("ephemeral_jobs_in_flight")
            .with_count(counts.total),
        ));
    }
    Ok(HookOutcome::pass())
}

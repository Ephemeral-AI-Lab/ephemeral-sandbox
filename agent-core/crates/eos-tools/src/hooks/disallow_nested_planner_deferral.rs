//! The disallow-nested-planner-deferral prehook — relocated out of the hooks
//! module into its own file (mirrors `hooks/advisor_approval.rs`
//! and `hooks/require_no_background_sessions.rs`), porting Python
//! `tools/_hooks/disallow_nested_planner_deferral.py`.
//!
//! It denies a planner terminal that carries a nonblank
//! `deferred_goal_for_next_iteration` when the submitting workflow's delegation
//! depth exceeds the configured `max_depth`. Depth is inferred from the workflow
//! context at hook execution via [`WorkflowControlPort::workflow_depth`]. A
//! too-deep planner cannot extend its iteration chain, bounding nesting.

use eos_types::JsonObject;
use serde_json::Value;

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;

use super::{HookDenial, HookOutcome};

const NESTED_PLANNER_DEFERRAL_MESSAGE: &str = "BLOCKED: nested workflow planners cannot set deferred_goal_for_next_iteration. Submit a plan that covers all current child-workflow goal items and leaves no remaining items.";

/// `DisallowNestedPlannerDeferral.run`: deny when a nonblank deferred goal is set
/// and the submitting workflow's depth exceeds `max_depth`. Passes when no
/// deferred goal is set, or when the workflow context (`workflow_id` +
/// `workflow_control`) is unavailable to infer depth.
pub(crate) async fn run_disallow_nested_planner_deferral(
    max_depth: u32,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
) -> Result<HookOutcome, ToolError> {
    let deferred = raw_input
        .get("deferred_goal_for_next_iteration")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if deferred.is_none() {
        return Ok(HookOutcome::pass());
    }
    let (Some(workflow_id), Some(control)) = (&ctx.workflow_id, &ctx.workflow_control) else {
        return Ok(HookOutcome::pass());
    };
    if control.workflow_depth(workflow_id).await? > max_depth {
        Ok(HookOutcome::Deny(
            HookDenial::new(NESTED_PLANNER_DEFERRAL_MESSAGE, "nested_planner_deferral")
                .with_reason("nested_workflow"),
        ))
    } else {
        Ok(HookOutcome::pass())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::ports::{OutstandingWorkflow, Sealed, StartedWorkflow, WorkflowControlPort};
    use crate::testsupport::metadata;
    use eos_types::{TaskId, WorkflowId, WorkflowSessionId};

    struct FixedDepth(u32);
    impl Sealed for FixedDepth {}

    #[async_trait]
    impl WorkflowControlPort for FixedDepth {
        async fn start(&self, _: &TaskId, _: &str, _: &str) -> Result<StartedWorkflow, ToolError> {
            unreachable!("depth hook never starts workflows")
        }

        async fn status(
            &self,
            _: &WorkflowId,
            _: Option<&WorkflowSessionId>,
        ) -> Result<String, ToolError> {
            unreachable!("depth hook never reads status")
        }

        async fn cancel(&self, _: &WorkflowSessionId, _: &str) -> Result<String, ToolError> {
            unreachable!("depth hook never cancels workflows")
        }

        async fn find_outstanding(
            &self,
            _: &TaskId,
            _: &str,
        ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
            unreachable!("depth hook never checks outstanding workflows")
        }

        async fn workflow_depth(&self, _: &WorkflowId) -> Result<u32, ToolError> {
            Ok(self.0)
        }
    }

    #[tokio::test]
    async fn denies_deferred_goal_when_depth_exceeds_max() {
        let mut input = JsonObject::new();
        input.insert(
            "deferred_goal_for_next_iteration".to_owned(),
            json!("finish child work"),
        );

        let mut ctx = metadata();
        ctx.workflow_id = Some(WorkflowId::new_v4());
        ctx.workflow_control = Some(Arc::new(FixedDepth(2)));

        let outcome = run_disallow_nested_planner_deferral(1, &input, &ctx)
            .await
            .expect("hook ran");

        match outcome {
            HookOutcome::Deny(denial) => {
                assert_eq!(denial.policy, "nested_planner_deferral");
                assert_eq!(denial.reason.as_deref(), Some("nested_workflow"));
                assert!(denial.message.contains("nested workflow planners"));
            }
            HookOutcome::Pass(_) => panic!("nested planner deferral should be denied"),
        }
    }

    #[tokio::test]
    async fn allows_deferred_goal_at_max_depth() {
        let mut input = JsonObject::new();
        input.insert(
            "deferred_goal_for_next_iteration".to_owned(),
            json!("finish child work"),
        );

        let mut ctx = metadata();
        ctx.workflow_id = Some(WorkflowId::new_v4());
        ctx.workflow_control = Some(Arc::new(FixedDepth(1)));

        let outcome = run_disallow_nested_planner_deferral(1, &input, &ctx)
            .await
            .expect("hook ran");

        assert!(matches!(outcome, HookOutcome::Pass(_)));
    }
}

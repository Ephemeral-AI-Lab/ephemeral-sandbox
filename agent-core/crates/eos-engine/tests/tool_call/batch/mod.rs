use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::ToolSpec;
use eos_tool::{
    ExecutionMetadata, OutputShape, RegisteredTool, ToolError, ToolExecutor, ToolName, ToolResult,
};
use eos_types::JsonObject;

use super::*;

struct NoopExecutor;

#[async_trait]
impl ToolExecutor for NoopExecutor {
    async fn execute(
        &self,
        _input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::ok(""))
    }
}

fn call<'a>(id: &'a str, name: &'a str) -> DispatchCall<'a> {
    DispatchCall {
        tool_use_id: id,
        name,
    }
}

fn registry_with(names: &[ToolName]) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for name in names {
        registry.register(RegisteredTool::new(
            *name,
            intent_for(*name),
            is_terminal(*name),
            ToolSpec::new(name.as_str(), "desc", JsonObject::new(), None),
            OutputShape::Text,
            Arc::new(NoopExecutor),
        ));
    }
    registry
}

fn intent_for(name: ToolName) -> ToolIntent {
    match name {
        ToolName::DelegateWorkflow
        | ToolName::CancelWorkflow
        | ToolName::EnterIsolatedWorkspace
        | ToolName::ExitIsolatedWorkspace => ToolIntent::Lifecycle,
        _ => ToolIntent::ReadOnly,
    }
}

fn is_terminal(name: ToolName) -> bool {
    matches!(
        name,
        ToolName::SubmitRootTaskOutcome
            | ToolName::SubmitPlanOutcome
            | ToolName::SubmitWorkerOutcome
            | ToolName::SubmitAdvisorOutcome
            | ToolName::SubmitSubagentOutcome
    )
}

// AC-tools-05: terminal-batch rejection rejects all calls; a solo terminal
// is allowed.
#[test]
fn terminal_batch_rejected() {
    let registry = registry_with(&[
        ToolName::ReadFile,
        ToolName::SubmitRootTaskOutcome,
        ToolName::EditFile,
    ]);

    // Solo terminal: allowed.
    assert!(reject_terminal_batch(&[call("t1", "submit_root_task_outcome")], &registry).is_none());

    // Terminal + sibling: every call rejected with the same message.
    let calls = [
        call("t1", "submit_root_task_outcome"),
        call("t2", "read_file"),
    ];
    let rejections = reject_terminal_batch(&calls, &registry).expect("rejected");
    assert_eq!(rejections.len(), 2);
    // Byte-exact verbatim contract (parity "EXACT Rejection Message"): flagged
    // is the sorted/deduped terminal set; called is every call in batch order.
    let expected = "Terminal tool `submit_root_task_outcome` must be called alone. This response \
         batched it with other tools: `submit_root_task_outcome`, `read_file`. No tool in this \
         batch executed. Resubmit with only the exclusive tool in its own final batch.";
    for rej in &rejections {
        assert_eq!(rej.message, expected, "verbatim terminal-batch message");
    }

    // No terminal in batch: allowed.
    assert!(reject_terminal_batch(
        &[call("t1", "read_file"), call("t2", "edit_file")],
        &registry
    )
    .is_none());
}

// AC-tools-06: >1 lifecycle rejects all lifecycle, keeps siblings; 1
// lifecycle + siblings rejects siblings, keeps the lifecycle call.
#[test]
fn lifecycle_batch_decision_policy() {
    let registry = registry_with(&[
        ToolName::DelegateWorkflow,
        ToolName::CancelWorkflow,
        ToolName::ReadFile,
    ]);

    // No lifecycle: dispatch all.
    let decision = lifecycle_batch_decision(&[call("a", "read_file")], &registry);
    assert!(decision.rejected.is_empty());
    assert_eq!(decision.dispatched, vec!["a"]);

    // >1 lifecycle: reject all lifecycle, sibling dispatches.
    let calls = [
        call("a", "delegate_workflow"),
        call("b", "cancel_workflow"),
        call("c", "read_file"),
    ];
    let decision = lifecycle_batch_decision(&calls, &registry);
    assert_eq!(decision.rejected.len(), 2);
    assert_eq!(decision.dispatched, vec!["c"]);
    // Byte-exact verbatim contract (parity "EXACT Message"); names are the
    // lifecycle calls in batch order.
    assert_eq!(
        decision.rejected[0].message,
        "Multiple lifecycle tools in one batch (`delegate_workflow`, `cancel_workflow`); \
         engine cannot choose ordering. Resubmit each lifecycle call in its own batch."
    );

    // 1 lifecycle + siblings: reject siblings, lifecycle dispatches solo.
    let calls = [call("a", "delegate_workflow"), call("b", "read_file")];
    let decision = lifecycle_batch_decision(&calls, &registry);
    assert_eq!(decision.dispatched, vec!["a"]);
    assert_eq!(decision.rejected.len(), 1);
    assert_eq!(decision.rejected[0].tool_use_id, "b");
    assert_eq!(
        decision.rejected[0].message,
        "`delegate_workflow` changes workspace routing; sibling tools (`read_file`) were \
         rejected to avoid ordering ambiguity. The lifecycle call executed. Resubmit the \
         rejected tools in the next batch."
    );
}

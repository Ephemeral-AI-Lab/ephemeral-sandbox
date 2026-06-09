//! The tool [`ExecutionMetadata`] fixture.
//!
//! Relocated from `eos-engine/src/support/test_support.rs` (`TESTING_SPEC` §7).
//! It stays in `eos-testkit` so engine in-crate tests can consume metadata
//! without crossing back into `eos-engine` types.

use std::sync::Arc;

use eos_tool::ExecutionMetadata;

/// A minimal [`ExecutionMetadata`] fact set for engine/tool tests.
#[must_use]
pub fn metadata() -> ExecutionMetadata {
    ExecutionMetadata {
        agent_name: "tester".to_owned(),
        agent_run_id: None,
        request_id: None,
        task_id: None,
        attempt_id: None,
        workflow_id: None,
        work_item_id: None,
        tool_use_id: None,
        sandbox_invocation_id: None,
        sandbox_id: None,
        is_isolated_workspace_mode: false,
        workspace_root: String::new(),
        conversation: Arc::from(Vec::new()),
    }
}

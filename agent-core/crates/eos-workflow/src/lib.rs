//! `eos-workflow` — delegated workflow lifecycle, per-attempt orchestration,
//! run-stage scheduling, launch context composition, and workflow-context
//! packets.
//!
//! The crate depends on store traits and downstream-state ports, not concrete
//! persistence or engine crates. It owns only delegated workflow state; root
//! requests remain direct root tasks.
#![forbid(unsafe_code)]

mod attempt;
mod context;
mod error;
mod ids;
mod iteration;
mod lifecycle;
mod ports;
mod starter;
mod util;

// Layer-B doubles (in-memory stores + `AgentRunner` doubles + `wait_until`).
// Kept crate-local under `tests/` (not `eos-testkit`): they are single-consumer
// `eos-workflow` types, so the dev-dep two-instance rule bars consuming them
// from this crate's own in-crate tests (`TESTING_SPEC` §14.2). Named `support` so
// the AC2 `mod (testsupport|test_support|…)` grep stays clean.
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

pub use attempt::{
    AgentLaunch, AgentRunReport, AgentRunner, AttemptDeps, AttemptOrchestratorRegistry,
    ExecutionLaunch, PlannerLaunch,
};
pub use context::{
    render_context_xml, render_task_guidance, AgentContext, AgentEntryComposer, AgentEntryMessages,
    ContextEngine, ContextEngineDeps, ContextRole, ContextScope, ContextSection,
};
pub use error::{Result, WorkflowError};
pub use ids::{generator_task_id, planner_task_id, reducer_task_id, WorkflowLifecycleConfig};
pub use iteration::OpenIterationCoordinatorRegistry;
pub use ports::{AttemptSubmissionAdapter, WorkflowControlAdapter};
pub use starter::{StartedWorkflow, WorkflowStarter};

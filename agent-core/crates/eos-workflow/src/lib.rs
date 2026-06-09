//! `eos-workflow` owns delegated workflow lifecycle, attempt scheduling, and
//! role-specific workflow context rendering.
//!
//! The crate depends on store traits and typed downstream contracts, not concrete
//! persistence or engine crates. Root requests remain direct root tasks.
#![forbid(unsafe_code)]

mod attempt;
mod attempt_submission;
mod config;
mod context;
mod error;
mod iteration_run;
mod workflow_run;

pub use attempt::OpenIterationCoordinatorRegistry;
pub use attempt::{
    ActiveAttemptRuns, AgentLaunch, AgentLaunchFactory, AgentLaunchKind, AgentRunReport,
    AgentRunner, AttemptResources, AttemptRun,
};
pub use attempt_submission::AttemptSubmissionAdapter;
pub use config::{
    AttemptConfig, WorkflowConfig, WorkflowLifecycleConfig, DEFAULT_WORKFLOW_MAX_DEPTH,
};
pub use context::{
    render_context_xml, render_task_guidance, AgentContext, ContextRole, ContextScope,
    ContextSection,
};
pub use error::{Result, WorkflowError};
pub use iteration_run::IterationRunCoordinator;
pub use workflow_run::{StartedWorkflowRun, WorkflowRun};

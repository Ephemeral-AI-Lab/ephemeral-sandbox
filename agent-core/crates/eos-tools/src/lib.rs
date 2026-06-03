//! `eos-tools` — the tool **model surface**: the typed [`ToolName`] set, the
//! [`ToolIntent`] classification, the [`ToolError`] framework-fault enum, the
//! [`ToolExecutor`] seam, the [`ToolRegistry`], the colocated `ToolSpec` sources
//! (one per model-facing tool), the terminal-descriptor catalog, the inner
//! [`execute_tool_once`] pipeline (parse → pre-hooks → execute → validate output →
//! stamp-terminal-on-success), and the pure batch-dispatch decision functions
//! ([`reject_terminal_batch`], [`lifecycle_batch_decision`]).
//!
//! It owns the *decisions*; `eos-engine` owns the async query/dispatch *loop*,
//! the background supervisor, stream events, and `ToolResultBlock`. Tools that
//! need downstream state depend on a **narrow [port trait](ports)** defined here
//! and implemented downstream. See
//! `docs/plans/backend_agent_core_rust_migration/impl-eos-tools.md`.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod dispatch;
mod error;
mod execution;
mod executor;
mod hooks;
mod intent;
mod meta;
mod metadata;
mod model_tools;
mod name;
pub mod ports;
mod registry;
mod result;
mod spec;
mod terminal;

#[cfg(test)]
mod testsupport;

pub use dispatch::{
    lifecycle_batch_decision, reject_terminal_batch, BatchRejection, DispatchCall,
    LifecycleBatchDecision,
};
pub use error::ToolError;
pub use execution::{execute_tool_once, run_pre_hooks};
pub use executor::{RegisteredTool, ToolExecutor};
pub use hooks::{Hook, HookDenial, HookOutcome};
pub use intent::ToolIntent;
pub use metadata::ExecutionMetadata;
pub use model_tools::{build_default_registry, CallerScope};
pub use name::ToolName;
pub use ports::{
    CommandSessionSupervisorPort, IsolatedWorkspacePort, NotificationSink, OutstandingWorkflow,
    PlanReducer, PlanSubmissionPort, PlanTask, PlannerPlan, StartedSubagent, StartedWorkflow,
    SubagentSupervisorPort, SubmissionAck, SystemNotification, WorkflowControlPort,
};
pub use registry::ToolRegistry;
pub use result::{OutputShape, ToolResult};
pub use terminal::{
    descriptor, render_tool_instruction, TerminalDescriptor, TerminalTool, ToolInstructions,
};

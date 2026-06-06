//! `eos-tools` — the tool **model surface**: the typed [`ToolName`] set, the
//! [`ToolIntent`] classification, the [`ToolError`] framework-fault enum, the
//! [`ToolExecutor`] seam, the [`ToolRegistry`], the colocated `ToolSpec` sources
//! (one per model-facing tool), the terminal-descriptor catalog, the inner
//! [`execute_tool_once`] pipeline (reject background → pre-hooks on raw input →
//! execute/body parse → validate output → stamp-terminal-on-success), and the
//! pure batch-dispatch decision functions
//! ([`reject_terminal_batch`], [`lifecycle_batch_decision`]).
//!
//! It owns the *decisions*; `eos-engine` owns the async query/dispatch *loop*,
//! the background supervisor, stream events, and `ToolResultBlock`. Tools that
//! need downstream state depend on a **narrow [port trait](ports)** defined here
//! and implemented downstream.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[path = "core/mod.rs"]
mod core;
#[path = "hooks/mod.rs"]
mod hooks;
#[path = "ports/mod.rs"]
pub mod ports;
#[path = "registry/mod.rs"]
mod registry;
#[path = "runtime/mod.rs"]
mod runtime;
#[path = "tools/mod.rs"]
mod tools;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

pub use core::error::ToolError;
pub use core::intent::ToolIntent;
pub use core::metadata::ExecutionMetadata;
pub use core::name::{ToolKey, ToolName};
pub use core::result::{OutputShape, ToolResult};
pub use hooks::{Hook, HookDenial, HookOutcome};
pub use ports::{
    AttemptSubmissionPort, BackgroundInflightReport, BackgroundSupervisorPort,
    CommandSessionSupervisorPort, NotificationSink, OutstandingWorkflow, PlanReducer, PlanTask,
    PlannerPlan, SpawnedSubagent, StartedSubagent, StartedWorkflowHandle, SubmissionAck,
    SystemNotification, WorkflowControlPort,
};
pub use registry::config::{ToolConfig, ToolConfigError, ToolConfigSet};
pub use registry::tool_registry::ToolRegistry;
pub use runtime::dispatch::{
    lifecycle_batch_decision, reject_terminal_batch, BatchRejection, DispatchCall,
    LifecycleBatchDecision,
};
pub use runtime::execution::{execute_tool_once, run_pre_hooks};
pub use runtime::executor::{RegisteredTool, ToolExecutor};
pub use tools::terminal::{
    descriptor, render_tool_instruction, TerminalDescriptor, TerminalTool, ToolInstructions,
};
pub use tools::{
    build_default_registry, build_default_registry_with_services, AttemptSubmissionService,
    CallerScope, CommandToolService, HookServices, RootSubmissionService, SandboxToolService,
    SkillToolService,
};

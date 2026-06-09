//! `eos-tool` — model-facing tool framework and concrete tools.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod hooks;
mod model;
mod registry;
mod tools;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

pub use error::ToolError;
pub use hooks::Hook;
pub use model::{
    BackgroundSessionCounts, ExecutionMetadata, OutputShape, SubagentLaunchRejection,
    SubagentSessionStatus, ToolIntent, ToolKey, ToolName, ToolResult,
};
pub use registry::{
    build_default_registry, build_registry_schema, BackgroundSessions, CallerScope, RegisteredTool,
    Submission, ToolConfig, ToolConfigError, ToolConfigSet, ToolExecutor, ToolRegistry,
    ToolRuntime, WorkspaceMode,
};
pub use tools::terminal::{render_tool_instruction, TerminalTool, ToolInstructions};
pub use tools::{
    ReferenceName, SkillDefinition, SkillLoadError, SkillName, SkillRegistry, SkillSource,
};

//! Background-tool policy.

use eos_tools::ToolName;

/// Whether a tool is an engine-dispatched background tool.
#[must_use]
pub const fn is_engine_background_tool(name: ToolName) -> bool {
    matches!(
        name,
        ToolName::RunSubagent | ToolName::ExecCommand | ToolName::DelegateWorkflow
    )
}

/// Whether the tool needs access to the background supervisor.
#[must_use]
pub const fn needs_background_manager(name: ToolName) -> bool {
    matches!(
        name,
        ToolName::RunSubagent
            | ToolName::CheckSubagentProgress
            | ToolName::CancelSubagent
            | ToolName::ExecCommand
            | ToolName::WriteStdin
            | ToolName::DelegateWorkflow
            | ToolName::CheckWorkflowStatus
            | ToolName::CancelWorkflow
    )
}

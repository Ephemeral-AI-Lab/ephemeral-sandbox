//! Runtime prompt fragments.

use std::collections::BTreeSet;

use eos_tools::{descriptor, TerminalTool, ToolName};

/// Build the termination-condition prompt appended at agent spawn.
#[must_use]
pub fn build_termination_condition_prompt(terminal_tools: &BTreeSet<ToolName>) -> String {
    if terminal_tools.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "When your assigned work is complete, call exactly one terminal tool in its own final message.".to_owned(),
        "Available terminal tools:".to_owned(),
    ];
    for tool_name in terminal_tools {
        if let Some(terminal) = TerminalTool::from_tool_name(*tool_name) {
            let desc = descriptor(terminal);
            lines.push(format!(
                "- `{}`: {}",
                desc.name.as_str(),
                desc.selection_guidance
            ));
        }
    }
    lines.join("\n")
}

use crate::core::name::ToolName;
use crate::core::result::ToolResult;
use crate::ports::SubagentSessionStatus;

#[cfg(test)]
#[path = "../../../tests/tools/subagent/mod.rs"]
mod tests;

pub(super) fn default_five() -> u8 {
    5
}

pub(super) fn empty_subagent_session_error(tool: ToolName) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: subagent_session_id must be non-empty. \
         Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

pub(super) fn subagent_status_and_result(
    status: SubagentSessionStatus,
    result: Option<&ToolResult>,
) -> (&'static str, String) {
    let metadata = result.map(|result| &result.metadata);
    if let Some(reason) = metadata
        .and_then(|m| m.get("subagent_termination_reason"))
        .and_then(serde_json::Value::as_str)
    {
        return ("terminated", format!("[terminated: {reason}] "));
    }
    if metadata
        .and_then(|m| m.get("subagent_cancelled"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return ("cancelled", "[cancelled] ".to_owned());
    }
    let output = || {
        result
            .map(|result| result.output.clone())
            .unwrap_or_default()
    };
    match status {
        SubagentSessionStatus::Running => ("running", String::new()),
        SubagentSessionStatus::Completed | SubagentSessionStatus::Delivered
            if terminal_called(result) =>
        {
            ("finished", output())
        }
        SubagentSessionStatus::Cancelled => ("cancelled", "[cancelled] ".to_owned()),
        _ => ("failed", output()),
    }
}

fn terminal_called(result: Option<&ToolResult>) -> bool {
    result
        .and_then(|result| result.metadata.get("subagent_terminal_called"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

use std::fs;

use eos_tool::{render_tool_instruction, ToolInstructions, ToolName};
use eos_types::AgentDefinition;

use crate::{Result, WorkflowError};

pub(crate) fn wrap_task_guidance(prose: &str, agent_def: &AgentDefinition) -> String {
    let body = prose.trim_end();
    if let Some(block) = terminal_selection_block(agent_def) {
        format!("<Task Guidance>\n{body}\n\n{block}\n</Task Guidance>")
    } else {
        format!("<Task Guidance>\n{body}\n</Task Guidance>")
    }
}

pub(crate) fn build_skill_message(agent_def: &AgentDefinition) -> Result<Option<String>> {
    let Some(path) = &agent_def.skill else {
        return Ok(None);
    };
    let raw =
        fs::read_to_string(path).map_err(|err| WorkflowError::AgentDefinition(err.to_string()))?;
    let body = strip_frontmatter(&raw).trim().to_owned();
    let skill_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("skill");
    let mut parts = vec![
        format!("Load skill: {skill_name}"),
        String::new(),
        "<skill>".to_owned(),
        body,
        "</skill>".to_owned(),
    ];
    if let Some(block) = terminal_selection_block(agent_def) {
        parts.push(String::new());
        parts.push(block);
    }
    Ok(Some(parts.join("\n")))
}

fn strip_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else {
        return raw;
    };
    let Some((_, body)) = rest.split_once("\n---") else {
        return raw;
    };
    body
}

fn terminal_selection_block(agent_def: &AgentDefinition) -> Option<String> {
    let terminals = agent_def
        .terminals
        .iter()
        .filter_map(|terminal| terminal.parse::<ToolName>().ok())
        .collect::<Vec<_>>();
    if terminals.is_empty() {
        None
    } else {
        let catalog = render_tool_instruction(&terminals, ToolInstructions::SelectionGuidance);
        Some(format!(
            "<terminal_tool_selection>\n{catalog}\n</terminal_tool_selection>"
        ))
    }
}

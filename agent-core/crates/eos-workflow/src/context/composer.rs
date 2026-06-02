use std::fs;
use std::sync::Arc;

use eos_agent_def::{AgentDefinition, AgentName, AgentRegistry};
use eos_tools::{descriptor, TerminalTool, ToolName};

use crate::{Result, WorkflowError};

use super::{render_context_xml, ContextEngine, ContextScope};
use super::{AgentContext, ContextRole};

/// Composed launch messages for one agent run.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEntryMessages {
    /// Resolved agent definition.
    pub agent_def: AgentDefinition,
    /// Rendered `<context>` row.
    pub context: String,
    /// Rendered `<Task Guidance>` row.
    pub task_guidance: Option<String>,
    /// Rendered skill-loading row.
    pub skill: Option<String>,
}

/// Agent-entry message composer.
#[derive(Clone)]
pub struct AgentEntryComposer {
    engine: ContextEngine,
    agents: Arc<AgentRegistry>,
}

impl std::fmt::Debug for AgentEntryComposer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEntryComposer").finish_non_exhaustive()
    }
}

impl AgentEntryComposer {
    /// Create a composer from the context engine and agent registry.
    #[must_use]
    pub fn new(engine: ContextEngine, agents: Arc<AgentRegistry>) -> Self {
        Self { engine, agents }
    }

    /// Compose the launch rows for `base_agent_name`.
    ///
    /// # Errors
    /// Returns [`WorkflowError`] when the agent is missing or lacks a matching
    /// context recipe.
    pub async fn compose(
        &self,
        base_agent_name: &str,
        scope: &ContextScope,
    ) -> Result<AgentEntryMessages> {
        let name = AgentName::new(base_agent_name)?;
        let agent_def = self
            .agents
            .get(&name)
            .ok_or_else(|| {
                WorkflowError::AgentDefinition(format!(
                    "agent definition {base_agent_name:?} is not registered"
                ))
            })?
            .as_ref()
            .clone();
        let recipe = agent_def.context_recipe.as_deref().ok_or_else(|| {
            WorkflowError::AgentDefinition(format!(
                "agent {:?} has no context_recipe declared",
                agent_def.name.as_str()
            ))
        })?;
        let context = self.engine.build(recipe, scope).await?;
        Ok(AgentEntryMessages {
            context: render_context_xml(&context),
            task_guidance: Some(wrap_task_guidance(
                &render_task_guidance(&context),
                &agent_def,
            )),
            skill: build_skill_message(&agent_def)?,
            agent_def,
        })
    }
}

/// Render role guidance from a context packet.
#[must_use]
pub fn render_task_guidance(context: &AgentContext) -> String {
    let contents = match context.role {
        ContextRole::Planner => [
            "- <workflow>: workflow goal and current planning frame",
            "- <prior_iterations>: reducer outcomes from prior iterations",
            "- <current_iteration>: current goal and previous attempt evidence",
        ]
        .as_slice(),
        ContextRole::Generator | ContextRole::Reducer => [
            "- <dependencies>: outcomes produced by dependency tasks",
            "- <assigned_task>: your assigned task",
        ]
        .as_slice(),
    };
    let mut parts = vec![format!("What's in context:\n{}", contents.join("\n"))];
    if !context.context_limits.is_empty() {
        parts.push(format!(
            "Context limits:\n{}",
            context
                .context_limits
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    parts.push(format!("What to do:\n- {}", context.directive));
    parts.join("\n\n")
}

fn wrap_task_guidance(prose: &str, agent_def: &AgentDefinition) -> String {
    let body = prose.trim_end();
    if let Some(block) = terminal_selection_block(agent_def) {
        format!("<Task Guidance>\n{body}\n\n{block}\n</Task Guidance>")
    } else {
        format!("<Task Guidance>\n{body}\n</Task Guidance>")
    }
}

fn build_skill_message(agent_def: &AgentDefinition) -> Result<Option<String>> {
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
    let mut rows = Vec::new();
    for terminal in &agent_def.terminals {
        let Ok(name) = terminal.parse::<ToolName>() else {
            continue;
        };
        let Some(term) = TerminalTool::from_tool_name(name) else {
            continue;
        };
        let desc = descriptor(term);
        rows.push(format!(
            "- {}: {}",
            desc.name.as_str(),
            desc.selection_guidance
        ));
    }
    if rows.is_empty() {
        None
    } else {
        Some(format!(
            "<terminal_tool_selection>\nPick exactly one based on outcome:\n\n{}\n</terminal_tool_selection>",
            rows.join("\n")
        ))
    }
}

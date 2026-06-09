use std::fs;
use std::sync::Arc;

use eos_tool::{render_tool_instruction, ToolInstructions, ToolName};
use eos_types::{AgentDefinition, AgentName, AgentRegistry};

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
    let mut terminals = Vec::new();
    for terminal in &agent_def.terminals {
        let Ok(name) = terminal.parse::<ToolName>() else {
            continue;
        };
        terminals.push(name);
    }
    if terminals.is_empty() {
        None
    } else {
        let catalog = render_tool_instruction(&terminals, ToolInstructions::SelectionGuidance);
        Some(format!(
            "<terminal_tool_selection>\n{catalog}\n</terminal_tool_selection>"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use eos_types::{AgentName, AgentType};

    use super::*;

    fn agent_def(terminals: Vec<&str>) -> AgentDefinition {
        AgentDefinition {
            name: AgentName::new("coder").expect("agent name"),
            description: "coder".to_owned(),
            system_prompt: None,
            model: None,
            tool_call_limit: NonZeroU32::new(8).expect("nonzero"),
            agent_type: AgentType::Agent,
            allowed_tools: Vec::new(),
            terminals: terminals.into_iter().map(ToOwned::to_owned).collect(),
            notification_triggers: Vec::new(),
            skill: None,
            context_recipe: Some("generator".to_owned()),
        }
    }

    #[test]
    fn terminal_selection_uses_terminal_catalog_format() {
        let terminal = ToolName::SubmitGeneratorOutcome;
        let expected_catalog =
            render_tool_instruction(&[terminal], ToolInstructions::SelectionGuidance);

        let block =
            terminal_selection_block(&agent_def(vec![terminal.as_str()])).expect("terminal block");

        assert_eq!(
            block,
            format!("<terminal_tool_selection>\n{expected_catalog}\n</terminal_tool_selection>")
        );
        assert!(!block.contains("Pick exactly one"));
    }

    fn engine() -> ContextEngine {
        let stores = Arc::new(crate::support::MemoryStores::default());
        ContextEngine::new(crate::context::ContextEngineDeps {
            workflow_store: stores.clone(),
            iteration_store: stores.clone(),
            attempt_store: stores.clone(),
            task_store: stores,
        })
    }

    #[test]
    fn strip_frontmatter_removes_yaml_block_and_passes_plain_through() {
        assert_eq!(strip_frontmatter("---\nname: x\n---\nBODY").trim(), "BODY");
        // No frontmatter fence -> returned unchanged.
        assert_eq!(strip_frontmatter("plain body"), "plain body");
        // An opening fence with no closing fence -> returned unchanged.
        assert_eq!(strip_frontmatter("---\nunterminated"), "---\nunterminated");
    }

    #[test]
    fn build_skill_message_reads_strips_frontmatter_and_derives_name() {
        let root = std::env::temp_dir().join(format!("eos-skill-{}", std::process::id()));
        let skill_dir = root.join("my-skill");
        std::fs::create_dir_all(&skill_dir).expect("mkdir skill dir");
        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, "---\nname: ignored\n---\nDo the thing.").expect("write skill");

        let mut def = agent_def(vec![]);
        def.skill = Some(skill_file);
        let message = build_skill_message(&def)
            .expect("build skill message")
            .expect("a skill message");
        assert!(message.contains("Load skill: my-skill"), "{message}");
        assert!(message.contains("<skill>"));
        assert!(message.contains("Do the thing."));
        assert!(message.contains("</skill>"));
        assert!(
            !message.contains("name: ignored"),
            "frontmatter is stripped from the skill body"
        );

        // No skill declared -> None.
        assert!(build_skill_message(&agent_def(vec![]))
            .expect("no skill is ok")
            .is_none());

        // A declared-but-missing skill file -> error.
        let mut missing = agent_def(vec![]);
        missing.skill = Some(root.join("absent").join("SKILL.md"));
        assert!(build_skill_message(&missing).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn wrap_task_guidance_wraps_body_and_appends_terminal_block_when_present() {
        let plain = wrap_task_guidance("BODY", &agent_def(vec![]));
        assert!(plain.starts_with("<Task Guidance>"));
        assert!(plain.contains("BODY"));
        assert!(plain.ends_with("</Task Guidance>"));
        assert!(
            !plain.contains("<terminal_tool_selection>"),
            "no terminals -> no terminal block"
        );

        let with_terminal = wrap_task_guidance(
            "BODY",
            &agent_def(vec![ToolName::SubmitGeneratorOutcome.as_str()]),
        );
        assert!(with_terminal.contains("<terminal_tool_selection>"));
    }

    #[tokio::test]
    async fn compose_rejects_missing_agent_and_missing_recipe() {
        let scope = ContextScope::for_planner(
            eos_types::WorkflowId::new_v4(),
            eos_types::IterationId::new_v4(),
            eos_types::AttemptId::new_v4(),
        );

        // Agent absent from the registry -> error (before the engine is touched).
        let empty = AgentEntryComposer::new(
            engine(),
            Arc::new(AgentRegistry::from_iter(Vec::<AgentDefinition>::new())),
        );
        assert!(empty.compose("ghost", &scope).await.is_err());

        // Agent present but with no context_recipe -> error.
        let mut no_recipe = agent_def(vec![]);
        no_recipe.name = AgentName::new("solo").expect("name");
        no_recipe.context_recipe = None;
        let composer =
            AgentEntryComposer::new(engine(), Arc::new(AgentRegistry::from_iter([no_recipe])));
        assert!(composer.compose("solo", &scope).await.is_err());
    }
}

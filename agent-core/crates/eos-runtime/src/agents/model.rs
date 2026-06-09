//! Loader-local raw frontmatter DTO and validation into `eos-types` agent DTOs.

use std::num::NonZeroU32;
use std::path::PathBuf;

pub use eos_types::{AgentDefinition, AgentName, AgentType};
use serde::Deserialize;

use super::error::AgentDefError;

/// The serde DTO for the YAML frontmatter block (`extra="forbid"` →
/// `#[serde(deny_unknown_fields)]`, GC-eos-agent-def-02).
///
/// The loader post-processes this (name/description defaults, contract prepend,
/// skill resolution) and then funnels it through [`definition_from_frontmatter`].
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAgentDefinition {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tool_call_limit: u32,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub terminals: Vec<String>,
    #[serde(default)]
    pub notification_triggers: Vec<String>,
    #[serde(default)]
    pub skill: Option<PathBuf>,
    #[serde(default)]
    pub context_recipe: Option<String>,
}

/// Validate a post-processed frontmatter DTO into an agent definition.
///
/// The loader supplies `path` for path-bearing errors and has already applied
/// the name/description defaults and resolved the skill path.
pub(crate) fn definition_from_frontmatter(
    raw: RawAgentDefinition,
) -> Result<AgentDefinition, AgentDefError> {
    let name =
        AgentName::new(raw.name.unwrap_or_default()).map_err(|_| AgentDefError::EmptyName)?;
    let tool_call_limit =
        NonZeroU32::new(raw.tool_call_limit).ok_or(AgentDefError::NonPositiveToolCallLimit)?;
    let terminals: Vec<String> = raw
        .terminals
        .into_iter()
        .filter(|terminal| !terminal.trim().is_empty())
        .collect();
    if terminals.is_empty() {
        return Err(AgentDefError::EmptyTerminals);
    }
    let notification_triggers = raw
        .notification_triggers
        .into_iter()
        .filter(|trigger| !trigger.trim().is_empty())
        .collect();
    Ok(AgentDefinition {
        name,
        description: raw.description.unwrap_or_default(),
        system_prompt: raw.system_prompt,
        model: raw.model,
        tool_call_limit,
        agent_type: raw.agent_type,
        allowed_tools: raw.allowed_tools,
        terminals,
        notification_triggers,
        skill: raw.skill,
        context_recipe: raw.context_recipe,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn valid_raw() -> RawAgentDefinition {
        RawAgentDefinition {
            name: Some("worker".to_owned()),
            description: Some("a worker".to_owned()),
            tool_call_limit: 10,
            terminals: vec!["submit_generator_outcome".to_owned()],
            ..RawAgentDefinition::default()
        }
    }

    #[test]
    fn definition_rejects_unknown_field() {
        let yaml = "name: x\ndescription: y\ntool_call_limit: 1\nterminals: [t]\nbogus_key: 1\n";
        let parsed = serde_yaml::from_str::<RawAgentDefinition>(yaml);
        assert!(parsed.is_err(), "deny_unknown_fields must reject bogus_key");
    }

    #[test]
    fn definition_enforces_terminals_and_limit() {
        let mut empty = valid_raw();
        empty.terminals = vec![];
        assert!(matches!(
            definition_from_frontmatter(empty),
            Err(AgentDefError::EmptyTerminals)
        ));

        let mut blank = valid_raw();
        blank.terminals = vec!["   ".to_owned(), "".to_owned()];
        assert!(matches!(
            definition_from_frontmatter(blank),
            Err(AgentDefError::EmptyTerminals)
        ));

        let mut zero = valid_raw();
        zero.tool_call_limit = 0;
        assert!(matches!(
            definition_from_frontmatter(zero),
            Err(AgentDefError::NonPositiveToolCallLimit)
        ));
    }

    #[test]
    fn from_frontmatter_strips_blank_triggers_and_terminals() {
        let mut raw = valid_raw();
        raw.terminals = vec!["  ".to_owned(), "submit_x".to_owned()];
        raw.notification_triggers = vec!["keep".to_owned(), "  ".to_owned()];
        let def = definition_from_frontmatter(raw).unwrap();
        assert_eq!(def.terminals, vec!["submit_x".to_owned()]);
        assert_eq!(def.notification_triggers, vec!["keep".to_owned()]);
    }

    #[test]
    fn blank_name_maps_to_agent_def_error() {
        let mut raw = valid_raw();
        raw.name = Some("   ".to_owned());
        assert!(matches!(
            definition_from_frontmatter(raw),
            Err(AgentDefError::EmptyName)
        ));
    }
}

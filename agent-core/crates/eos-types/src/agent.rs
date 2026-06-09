//! Passive agent definition DTOs and read-only registry.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Runtime class of an agent profile.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    /// A regular agent.
    #[default]
    Agent,
    /// A worker subagent targetable by `run_subagent`.
    Subagent,
    /// A blocking read-only advisor targetable by `ask_advisor`.
    Advisor,
}

/// A registry key / dispatchable agent profile name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct AgentName(String);

impl AgentName {
    /// Construct a name, trimming surrounding whitespace.
    ///
    /// # Errors
    /// Returns [`AgentNameError::Empty`] when the trimmed value is empty.
    pub fn new(raw: impl AsRef<str>) -> Result<Self, AgentNameError> {
        let trimmed = raw.as_ref().trim().to_owned();
        if trimmed.is_empty() {
            return Err(AgentNameError::Empty);
        }
        Ok(Self(trimmed))
    }

    /// Borrow the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Agent-name validation error.
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentNameError {
    /// Agent names must not be empty after trimming.
    #[error("agent name must be non-empty")]
    Empty,
}

/// Full agent definition with all configuration fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    /// Registry key and dispatchable name.
    pub name: AgentName,
    /// Human-readable description.
    pub description: String,
    /// Composed system prompt; `None` when the profile declares none.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Raw model id; the `"inherit"` sentinel is resolved downstream.
    #[serde(default)]
    pub model: Option<String>,
    /// Per-run cap on tool dispatches.
    pub tool_call_limit: NonZeroU32,
    /// Regular agent, worker subagent, or advisor helper.
    #[serde(default)]
    pub agent_type: AgentType,
    /// Tools the agent may call.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Terminal tools that end the query loop.
    pub terminals: Vec<String>,
    /// Declarative notification-trigger ids.
    #[serde(default)]
    pub notification_triggers: Vec<String>,
    /// Absolute path to the agent's workflow skill, resolved by the loader.
    #[serde(default)]
    pub skill: Option<PathBuf>,
    /// Context-engine recipe id resolved at compose time.
    #[serde(default)]
    pub context_recipe: Option<String>,
}

/// Accumulates definitions, then finalizes an immutable [`AgentRegistry`].
#[derive(Debug, Default)]
#[must_use]
pub struct AgentRegistryBuilder {
    definitions: HashMap<AgentName, Arc<AgentDefinition>>,
}

impl AgentRegistryBuilder {
    /// Start an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace a definition by its name.
    pub fn add(&mut self, definition: AgentDefinition) -> &mut Self {
        self.definitions
            .insert(definition.name.clone(), Arc::new(definition));
        self
    }

    /// Finalize the immutable registry.
    #[must_use]
    pub fn build(self) -> AgentRegistry {
        AgentRegistry {
            definitions: self.definitions,
        }
    }
}

impl FromIterator<AgentDefinition> for AgentRegistry {
    fn from_iter<I: IntoIterator<Item = AgentDefinition>>(iter: I) -> Self {
        let mut builder = AgentRegistryBuilder::new();
        for definition in iter {
            builder.add(definition);
        }
        builder.build()
    }
}

/// Immutable name-to-definition lookup shared by runtime and workflow.
#[derive(Debug)]
pub struct AgentRegistry {
    definitions: HashMap<AgentName, Arc<AgentDefinition>>,
}

impl AgentRegistry {
    /// Look up a definition by name.
    #[must_use]
    pub fn get(&self, name: &AgentName) -> Option<&Arc<AgentDefinition>> {
        self.definitions.get(name)
    }

    /// Iterate every registered definition.
    pub fn list(&self) -> impl Iterator<Item = &Arc<AgentDefinition>> {
        self.definitions.values()
    }

    /// Subagent names targetable by `run_subagent`, sorted.
    #[must_use]
    pub fn dispatchable_subagent_names(&self) -> Vec<AgentName> {
        let mut names: Vec<AgentName> = self
            .definitions
            .values()
            .filter(|definition| definition.agent_type == AgentType::Subagent)
            .map(|definition| definition.name.clone())
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::num::NonZeroU32;

    use super::*;

    fn def(name: &str, agent_type: AgentType) -> AgentDefinition {
        AgentDefinition {
            name: AgentName::new(name).unwrap(),
            description: "d".to_owned(),
            system_prompt: None,
            model: None,
            tool_call_limit: NonZeroU32::new(10).unwrap(),
            agent_type,
            allowed_tools: vec![],
            terminals: vec!["submit_x".to_owned()],
            notification_triggers: vec![],
            skill: None,
            context_recipe: None,
        }
    }

    #[test]
    fn agent_type_serde_values() {
        let agent = serde_json::to_value(AgentType::Agent).unwrap();
        assert_eq!(agent, serde_json::json!("agent"));
        let subagent = serde_json::to_value(AgentType::Subagent).unwrap();
        assert_eq!(subagent, serde_json::json!("subagent"));
        let advisor = serde_json::to_value(AgentType::Advisor).unwrap();
        assert_eq!(advisor, serde_json::json!("advisor"));
    }

    #[test]
    fn agent_name_trims_and_rejects_empty() {
        assert_eq!(AgentName::new("  root  ").unwrap().as_str(), "root");
        assert!(matches!(AgentName::new("   "), Err(AgentNameError::Empty)));

        let deserialized: AgentName = serde_json::from_value(serde_json::json!("  root  "))
            .expect("agent name serde trims like the constructor");
        assert_eq!(deserialized.as_str(), "root");
        assert!(serde_json::from_value::<AgentName>(serde_json::json!("   ")).is_err());
    }

    #[test]
    fn registry_lists_dispatchable_subagents() {
        let registry: AgentRegistry = [
            def("zeta", AgentType::Subagent),
            def("root", AgentType::Agent),
            def("alpha", AgentType::Subagent),
        ]
        .into_iter()
        .collect();

        let names: Vec<String> = registry
            .dispatchable_subagent_names()
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["alpha".to_owned(), "zeta".to_owned()]);
    }

    #[test]
    fn registry_get_and_replace() {
        let mut builder = AgentRegistryBuilder::new();
        builder.add(def("root", AgentType::Agent));
        builder.add(def("root", AgentType::Agent));
        let registry = builder.build();
        assert!(registry.get(&AgentName::new("root").unwrap()).is_some());
        assert_eq!(registry.list().count(), 1);
        assert!(registry.get(&AgentName::new("absent").unwrap()).is_none());
    }
}

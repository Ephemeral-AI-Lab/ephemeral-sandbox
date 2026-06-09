//! Agent-definition builders for Layer-A tests.

use std::num::NonZeroU32;

use eos_types::{AgentDefinition, AgentName, AgentType};

/// Build a minimal agent definition for tests.
#[must_use]
pub fn agent_def(name: &str, allowed: &[&str], terminals: &[&str]) -> AgentDefinition {
    AgentDefinition {
        name: AgentName::new(name).expect("name"),
        description: name.to_owned(),
        system_prompt: Some("test profile".to_owned()),
        model: Some("test-model".to_owned()),
        tool_call_limit: NonZeroU32::new(8).expect("nonzero"),
        agent_type: AgentType::Agent,
        allowed_tools: allowed.iter().map(|s| (*s).to_owned()).collect(),
        terminals: terminals.iter().map(|s| (*s).to_owned()).collect(),
        notification_triggers: Vec::new(),
        skill: None,
        context_recipe: None,
    }
}

/// The repo's `.eos-agents/tools` tree, resolved relative to this crate's
/// manifest so the (mandatory) tool-config build path has a real source in tests
/// without depending on the process working directory.
#[must_use]
pub fn test_tools_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.eos-agents/tools")
}

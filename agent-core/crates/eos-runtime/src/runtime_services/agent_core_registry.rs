//! Agent-core registry service group.

use std::sync::Arc;

use eos_tool::SkillRegistry;
use eos_tool::ToolConfigSet;
use eos_types::AgentRegistry;

/// Runtime registries and model-facing tool configuration.
#[derive(Clone)]
pub(crate) struct AgentCoreRegistryService {
    pub(crate) agent_registry: Arc<AgentRegistry>,
    pub(crate) skill_registry: Arc<SkillRegistry>,
    pub(crate) tool_config: Arc<ToolConfigSet>,
}

impl std::fmt::Debug for AgentCoreRegistryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCoreRegistryService")
            .field("agents", &self.agent_registry.list().count())
            .finish_non_exhaustive()
    }
}

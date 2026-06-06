//! Agent-core registry service group.

use std::sync::Arc;

use eos_agent_def::AgentRegistry;
use eos_skills::SkillRegistry;
use eos_tools::ToolConfigSet;

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

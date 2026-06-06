//! Skill-reference tools.

mod load_skill_reference;

use super::CallerScope;

pub(crate) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    caller: &CallerScope,
    skill_service: super::SkillToolService,
) {
    load_skill_reference::register(registry, config, caller, skill_service);
}

//! The model-facing tools: backend-shaped category folders with one file per
//! model-callable tool. Each tool authors its Input/Output DTOs and a
//! [`ToolExecutor`](crate::ToolExecutor) impl; the description and the intent /
//! terminal flag / hooks come from the externalized
//! [`ToolConfigSet`](crate::registry::config::ToolConfigSet) (`.eos-agents/tools/*.md`),
//! which the registry builder stamps onto each `RegisteredTool`.

mod ask_helper;
mod isolated_workspace;
mod sandbox;
mod services;
mod skills;
mod subagent;
mod submission;
pub(crate) mod terminal;
mod workflow;

use std::sync::Arc;

use eos_llm_client::ToolSpec;

use crate::core::name::ToolName;
use crate::core::result::OutputShape;
use crate::registry::config::{ToolConfig, ToolConfigSet};
use crate::registry::ToolRegistry;
use crate::runtime::executor::{RegisteredTool, ToolExecutor};

use services::InertSandboxTransport;
pub use services::{
    AttemptSubmissionService, CommandToolService, HookServices, RootSubmissionService,
    SandboxToolService, SkillToolService,
};

/// The per-caller scope a tool registry is built for: the caller's
/// dispatchable-subagent allow-list (which patches the `run_subagent` input
/// schema's `agent_name` enum) and the bound agent's own skill slug (which scopes
/// `load_skill_reference` to that one skill).
#[derive(Debug, Clone, Default)]
pub struct CallerScope {
    /// The subagent profile names this caller may dispatch.
    pub dispatchable_subagents: Vec<String>,
    /// The bound agent's own skill folder slug, if it declares one. Scopes
    /// `load_skill_reference` so the caller can read only that skill's references.
    /// `None` ⇒ a no-op tool.
    pub skill_slug: Option<String>,
}

/// Register one tool, stamping its intent / terminal flag / hooks from the loaded
/// [`ToolConfig`] so each registration site is a single line.
pub(crate) fn register_tool(
    registry: &mut ToolRegistry,
    name: ToolName,
    config: &ToolConfig,
    spec: ToolSpec,
    output: OutputShape,
    executor: Arc<dyn ToolExecutor>,
) {
    registry.register(
        RegisteredTool::new(name, config.intent, config.terminal, spec, output, executor)
            .with_hooks(config.hooks.clone()),
    );
}

/// Build the default tool registry for one caller scope from the externalized tool
/// config. Insertion order is stable because it backs the schema snapshot and is
/// the agent-spawn default before `restrict`/`remove`.
#[must_use]
pub fn build_default_registry(config: &ToolConfigSet, caller: &CallerScope) -> ToolRegistry {
    build_default_registry_with_services(
        config,
        caller,
        SandboxToolService::new(Arc::new(InertSandboxTransport)),
        None,
        None,
        None,
        None,
        None,
        SkillToolService::new(Arc::new(eos_skills::SkillRegistry::new())),
    )
}

/// Build the default tool registry for one caller scope with locally wired
/// services. Runtime uses this entry point; tests that only need static registry
/// policy can use [`build_default_registry`].
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_default_registry_with_services(
    config: &ToolConfigSet,
    caller: &CallerScope,
    sandbox_service: SandboxToolService,
    root_submission: Option<RootSubmissionService>,
    attempt_submission: Option<AttemptSubmissionService>,
    workflow_control: Option<Arc<dyn crate::ports::WorkflowControlPort>>,
    background_supervisor: Option<Arc<dyn crate::ports::BackgroundSupervisorPort>>,
    command_session_supervisor: Option<Arc<dyn crate::ports::CommandSessionSupervisorPort>>,
    skill_service: SkillToolService,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let hook_services = HookServices::new(
        Some(sandbox_service.transport()),
        workflow_control.clone(),
        background_supervisor.clone(),
    );
    let command_service =
        CommandToolService::new(sandbox_service.transport(), command_session_supervisor);
    sandbox::register(
        &mut registry,
        config,
        sandbox_service.clone(),
        command_service,
    );
    isolated_workspace::register(&mut registry, config, sandbox_service);
    submission::register(&mut registry, config, root_submission, attempt_submission);
    ask_helper::register(&mut registry, config);
    workflow::register(
        &mut registry,
        config,
        workflow_control,
        background_supervisor.clone(),
    );
    subagent::register(&mut registry, config, caller, background_supervisor);
    skills::register(&mut registry, config, caller, skill_service);
    registry.apply_hook_services(hook_services);
    registry
}

#[cfg(test)]
pub(crate) fn repo_tools_config() -> ToolConfigSet {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.eos-agents/tools");
    ToolConfigSet::load_from_dir(&root).expect("repo .eos-agents/tools loads and validates")
}

#[cfg(test)]
#[path = "../../tests/tools/mod.rs"]
mod tests;

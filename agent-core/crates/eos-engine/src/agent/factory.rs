//! Build a query context from an agent definition and injected runtime seams.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use eos_agent_def::AgentDefinition;
use eos_audit::AuditSink;
use eos_llm_client::{LlmClient, ReasoningEffort};
use eos_tools::{ExecutionMetadata, ToolKey, ToolName, ToolRegistry};
use eos_types::{AgentRunId, TaskId};

use crate::notifications::{make_default_notification_rules, NotificationService};
use crate::prompt::build_termination_condition_prompt;
use crate::query::{EventSource, ProviderEventSource, QueryContext};
use crate::EngineError;

/// Inputs for [`build_query_context`].
pub struct BuildQueryContextInput {
    /// Agent definition.
    pub agent: AgentDefinition,
    /// Resolved model key.
    pub model: String,
    /// Provider client for production event source.
    pub client: Option<Arc<dyn LlmClient>>,
    /// Explicit event-source override for tests.
    pub event_source: Option<Arc<dyn EventSource>>,
    /// Registry before per-agent restriction.
    pub registry: ToolRegistry,
    /// Runtime base prompt.
    pub base_system_prompt: String,
    /// Max completion tokens.
    pub max_tokens: u32,
    /// Provider reasoning-effort hint.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Agent run id.
    pub agent_run_id: AgentRunId,
    /// Owning task id.
    pub task_id: Option<TaskId>,
    /// Tool execution metadata.
    pub tool_metadata: ExecutionMetadata,
    /// The per-request notification sink shared with the tools/heartbeat. The
    /// loop drains this concrete handle each turn (anchor §7 instance identity).
    pub notifier: NotificationService,
    /// Optional agent-core observability sink.
    pub audit: Option<Arc<dyn AuditSink>>,
    /// The explicit run handles carried onto the [`QueryContext`] so the
    /// engine-driven advisor dispatch can spawn a child `run_agent`
    /// (advisor remediation plan §2a). `None` for runs that never advise.
    pub run_handles: Option<crate::EngineRunHandles>,
}

impl std::fmt::Debug for BuildQueryContextInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildQueryContextInput")
            .field("agent", &self.agent.name)
            .field("model", &self.model)
            .field("base_system_prompt", &self.base_system_prompt)
            .field("max_tokens", &self.max_tokens)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("cwd", &self.cwd)
            .field("agent_run_id", &self.agent_run_id)
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

fn parse_tool_keys(names: &[String]) -> Result<Vec<ToolKey>, EngineError> {
    names
        .iter()
        .map(|name| {
            ToolKey::from_wire(name).ok_or_else(|| EngineError::UnknownTool(name.to_owned()))
        })
        .collect()
}

fn parse_terminal_names(names: &[String]) -> Result<Vec<ToolName>, EngineError> {
    names
        .iter()
        .map(|name| {
            ToolName::from_wire(name).ok_or_else(|| EngineError::UnknownTool(name.to_owned()))
        })
        .collect()
}

/// Assemble one run's [`QueryContext`].
///
/// # Errors
/// Returns [`EngineError::UnknownTool`] if the agent names a tool outside the
/// typed tool catalog.
pub fn build_query_context(input: BuildQueryContextInput) -> Result<QueryContext, EngineError> {
    let BuildQueryContextInput {
        agent,
        model,
        client,
        event_source,
        mut registry,
        base_system_prompt,
        max_tokens,
        reasoning_effort,
        cwd,
        agent_run_id,
        task_id,
        tool_metadata,
        notifier,
        audit,
        run_handles,
    } = input;

    let mut allowed = parse_tool_keys(&agent.allowed_tools)?;
    let terminal_vec = parse_terminal_names(&agent.terminals)?;
    allowed.extend(terminal_vec.iter().copied().map(ToolKey::from));
    allowed.sort_unstable();
    allowed.dedup();
    if !allowed.is_empty() {
        registry.restrict(&allowed);
    }

    let terminal_tools: BTreeSet<ToolName> = terminal_vec.into_iter().collect();
    for terminal in &terminal_tools {
        let Some(tool) = registry.get(*terminal) else {
            return Err(EngineError::UnknownTool(terminal.as_str().to_owned()));
        };
        if !tool.is_terminal {
            return Err(EngineError::Internal(format!(
                "`{}` is listed as terminal but registry metadata is non-terminal",
                terminal.as_str()
            )));
        }
    }

    let mut prompt_parts = Vec::new();
    if !base_system_prompt.trim().is_empty() {
        prompt_parts.push(base_system_prompt);
    }
    if let Some(system_prompt) = agent.system_prompt.clone() {
        if !system_prompt.trim().is_empty() {
            prompt_parts.push(system_prompt);
        }
    }
    let terminal_prompt = build_termination_condition_prompt(&terminal_tools);
    if !terminal_prompt.trim().is_empty() {
        prompt_parts.push(terminal_prompt);
    }
    let event_source = event_source.or_else(|| {
        client.map(|client| Arc::new(ProviderEventSource::new(client)) as Arc<dyn EventSource>)
    });

    Ok(QueryContext {
        tool_registry: Arc::new(registry),
        cwd,
        model,
        system_prompt: prompt_parts.join("\n\n"),
        max_tokens,
        reasoning_effort,
        tool_call_limit: agent.tool_call_limit.get(),
        agent_name: agent.name.as_str().to_owned(),
        agent_run_id,
        task_id,
        tool_calls_used: 0,
        text_only_no_terminal_turns: 0,
        tool_metadata,
        terminal_tools,
        exit_reason: None,
        terminal_result: None,
        event_source,
        prompt_report: None,
        notification_rules: make_default_notification_rules(),
        notification_fired: BTreeSet::new(),
        notifier,
        audit,
        run_handles,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::num::NonZeroU32;

    use eos_agent_def::{AgentName, AgentRole, AgentType};
    use eos_llm_client::ToolSpec;
    use eos_tools::{OutputShape, RegisteredTool, ToolExecutor, ToolIntent, ToolResult};
    use eos_types::JsonObject;
    use serde_json::json;

    use super::*;
    use eos_testkit::metadata;

    #[derive(Debug)]
    struct Noop;

    #[async_trait::async_trait]
    impl ToolExecutor for Noop {
        async fn execute(
            &self,
            _input: &JsonObject,
            _ctx: &ExecutionMetadata,
        ) -> Result<ToolResult, eos_tools::ToolError> {
            Ok(ToolResult::ok("ok"))
        }
    }

    fn spec(name: ToolName) -> ToolSpec {
        ToolSpec::new(
            name.as_str(),
            "test",
            json!({"type":"object"})
                .as_object()
                .expect("object")
                .clone(),
            None,
        )
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(RegisteredTool::new(
            ToolName::ReadFile,
            ToolIntent::ReadOnly,
            false,
            spec(ToolName::ReadFile),
            OutputShape::Text,
            Arc::new(Noop),
        ));
        registry.register(RegisteredTool::new(
            ToolName::SubmitRootOutcome,
            ToolIntent::ReadOnly,
            true,
            spec(ToolName::SubmitRootOutcome),
            OutputShape::Text,
            Arc::new(Noop),
        ));
        registry
    }

    fn agent() -> AgentDefinition {
        AgentDefinition {
            name: AgentName::new("root").expect("name"),
            description: "root".to_owned(),
            system_prompt: Some("profile prompt".to_owned()),
            model: None,
            tool_call_limit: NonZeroU32::new(8).expect("nonzero"),
            role: AgentRole::Root,
            agent_type: AgentType::Agent,
            allowed_tools: vec!["read_file".to_owned()],
            terminals: vec!["submit_root_outcome".to_owned()],
            notification_triggers: Vec::new(),
            skill: None,
            context_recipe: None,
        }
    }

    #[test]
    fn factory_assembles_terminals_prompt_and_rules() {
        let ctx = build_query_context(BuildQueryContextInput {
            agent: agent(),
            model: "model".to_owned(),
            client: None,
            event_source: None,
            registry: registry(),
            base_system_prompt: "base".to_owned(),
            max_tokens: 32,
            reasoning_effort: None,
            cwd: PathBuf::new(),
            agent_run_id: AgentRunId::new_v4(),
            task_id: None,
            tool_metadata: metadata(),
            notifier: NotificationService::new(),
            audit: None,
            run_handles: None,
        })
        .expect("context");

        assert!(ctx.terminal_tools.contains(&ToolName::SubmitRootOutcome));
        assert!(ctx.system_prompt.contains("profile prompt"));
        assert!(ctx.system_prompt.contains("submit_root_outcome"));
        assert_eq!(ctx.notification_rules.len(), 4);
        assert_eq!(ctx.tool_registry.len(), 2);
    }
}

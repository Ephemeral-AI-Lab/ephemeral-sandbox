//! Streaming tool execution deferral policy.

use eos_tools::ToolName;

use crate::query::QueryContext;

/// Tracks whether mid-stream tool execution is enabled for a run.
#[derive(Debug, Clone, Default)]
pub struct StreamingToolExecutor {
    deferred: Vec<ToolName>,
}

impl StreamingToolExecutor {
    /// Create an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a tool was deferred.
    pub fn defer(&mut self, name: ToolName) {
        self.deferred.push(name);
    }

    /// Deferred tool names, in observation order.
    #[must_use]
    pub fn deferred(&self) -> &[ToolName] {
        &self.deferred
    }
}

/// Whether a tool should defer until the full assistant message is available.
#[must_use]
pub fn should_defer_tool(ctx: &QueryContext, _name: ToolName) -> bool {
    !ctx.terminal_tools.is_empty()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use eos_tools::{ToolName, ToolRegistry};
    use eos_types::{AgentRunId, JsonObject};

    use super::*;
    use crate::test_support::metadata;

    fn ctx(terminal_tools: BTreeSet<ToolName>) -> QueryContext {
        QueryContext {
            tool_registry: Arc::new(ToolRegistry::new()),
            cwd: PathBuf::new(),
            model: "m".to_owned(),
            system_prompt: String::new(),
            max_tokens: 1,
            tool_call_limit: 1,
            agent_name: "root".to_owned(),
            agent_run_id: AgentRunId::new_v4(),
            task_id: None,
            tool_calls_used: 0,
            text_only_no_terminal_turns: 0,
            tool_metadata: metadata(),
            enable_background_tasks: true,
            terminal_tools,
            exit_reason: None,
            terminal_result: None,
            event_source: None,
            prompt_report: None,
            notification_rules: Vec::new(),
            notification_fired: BTreeSet::new(),
            notification_state: JsonObject::new(),
        }
    }

    #[test]
    fn defer_all_when_terminal_present() {
        let with_terminal = ctx(BTreeSet::from([ToolName::SubmitRootOutcome]));
        assert!(should_defer_tool(&with_terminal, ToolName::ReadFile));
        assert!(should_defer_tool(
            &with_terminal,
            ToolName::SubmitRootOutcome
        ));

        let without_terminal = ctx(BTreeSet::new());
        assert!(!should_defer_tool(&without_terminal, ToolName::ReadFile));
    }
}

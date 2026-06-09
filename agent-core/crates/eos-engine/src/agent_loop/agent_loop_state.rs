//! Mutable state for one agent loop.

use std::collections::BTreeSet;
use std::sync::Arc;

use eos_llm_client::{ContentBlock, Message, MessageRole};
use eos_tool::{ToolKey, ToolRegistry, ToolResult};
use eos_types::{AgentRunApi, AgentRunId};

use crate::background::{BackgroundManagers, BackgroundSessionTeardown};
use crate::notifications::{
    enqueue_notification_rules, make_default_notification_rules, EngineNotificationQueue,
    NotificationRule, NotificationRuleContext, SystemNotification,
};

use super::{
    tool_result_payload, AgentLoopMessage, AgentLoopOutcome, AgentLoopOutcomeKind,
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, StartAgentLoopRequest,
};
use crate::EngineError;

/// Engine-private mutable loop state.
pub(crate) struct AgentLoopState {
    /// Agent-run id.
    pub(crate) agent_run_id: AgentRunId,
    /// Loop transcript.
    pub(crate) conversation_messages: Vec<AgentLoopMessage>,
    /// Resolved model key.
    pub(crate) model_key: String,
    /// Completion token cap.
    pub(crate) max_completion_tokens: u32,
    /// Tool-call limit.
    pub(crate) tool_call_limit: u32,
    /// Concrete registry for this loop.
    pub(crate) tool_registry: ToolRegistry,
    /// Total provider token count when known.
    pub(crate) total_token_count: Option<i64>,
    /// Counted model-requested tool calls.
    pub(crate) tool_calls_used: u32,
    /// Counted text-only turns without terminal submission.
    pub(crate) text_only_no_terminal_turns: u32,
    /// Run-local notification queue drained at loop turn boundaries.
    pub(crate) notifier: EngineNotificationQueue,
    /// Terminal tools visible to this agent loop.
    terminal_tools: BTreeSet<ToolKey>,
    /// Declarative notification rules.
    notification_rules: Vec<Arc<dyn NotificationRule>>,
    /// Fire-once notification names already emitted.
    notification_fired: BTreeSet<String>,
    /// Run-local background managers whose completions feed the notifier.
    background: Option<BackgroundManagers>,
    /// Run-local background teardown service.
    background_teardown: Option<BackgroundSessionTeardown>,
}

impl std::fmt::Debug for AgentLoopState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopState")
            .field("agent_run_id", &self.agent_run_id)
            .field("conversation_messages", &self.conversation_messages.len())
            .field("model_key", &self.model_key)
            .field("max_completion_tokens", &self.max_completion_tokens)
            .field("tool_call_limit", &self.tool_call_limit)
            .field("tool_registry_len", &self.tool_registry.len())
            .field("total_token_count", &self.total_token_count)
            .field("tool_calls_used", &self.tool_calls_used)
            .field(
                "text_only_no_terminal_turns",
                &self.text_only_no_terminal_turns,
            )
            .field("terminal_tools", &self.terminal_tools)
            .finish_non_exhaustive()
    }
}

impl AgentLoopState {
    pub(crate) fn from_request(
        request: StartAgentLoopRequest,
        tool_registry_factory: &dyn AgentLoopToolRegistryFactory,
        run_services: AgentLoopRunServices,
        agent_run_api: Arc<dyn AgentRunApi>,
    ) -> Result<Self, EngineError> {
        let tool_registry =
            tool_registry_factory.build_tool_registry(AgentLoopToolRegistryBuildInput {
                agent_run_id: request.agent_run_id.clone(),
                agent_run_api,
                background: run_services.background.clone(),
            })?;
        let terminal_tools = tool_registry
            .list()
            .filter(|tool| tool.is_terminal)
            .map(|tool| tool.name.clone())
            .collect();
        Ok(Self {
            agent_run_id: request.agent_run_id,
            conversation_messages: request.initial_messages,
            model_key: request.model_key,
            max_completion_tokens: request.max_completion_tokens,
            tool_call_limit: request.tool_call_limit,
            tool_registry,
            total_token_count: None,
            tool_calls_used: 0,
            text_only_no_terminal_turns: 0,
            notifier: run_services.notifier,
            terminal_tools,
            notification_rules: make_default_notification_rules(),
            notification_fired: BTreeSet::new(),
            background: run_services.background,
            background_teardown: run_services.background_teardown,
        })
    }

    pub(crate) fn record_tool_calls(&mut self, count: usize) {
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        self.tool_calls_used = self.tool_calls_used.saturating_add(count);
    }

    pub(crate) fn record_text_only_turn(&mut self) {
        self.text_only_no_terminal_turns = self.text_only_no_terminal_turns.saturating_add(1);
    }

    pub(crate) fn terminal_tool_submitted(self, outcome: ToolResult) -> AgentLoopOutcome {
        AgentLoopOutcome {
            kind: AgentLoopOutcomeKind::TerminalToolSubmitted {
                submission_payload: tool_result_payload(&outcome),
            },
            final_conversation_messages: self.conversation_messages,
            total_token_count: self.total_token_count,
        }
    }

    pub(crate) fn loop_failed(self, error: EngineError) -> AgentLoopOutcome {
        self.loop_failed_summary(error.to_string())
    }

    pub(crate) fn loop_failed_summary(self, error_summary: String) -> AgentLoopOutcome {
        AgentLoopOutcome {
            kind: AgentLoopOutcomeKind::LoopFailed { error_summary },
            final_conversation_messages: self.conversation_messages,
            total_token_count: self.total_token_count,
        }
    }

    pub(crate) fn turn_limit_reached(&self) -> bool {
        self.tool_calls_used
            .saturating_add(self.text_only_no_terminal_turns)
            >= self.hard_no_terminal_ceiling()
    }

    pub(crate) fn terminal_not_submitted_summary(&self) -> String {
        format!(
            "Agent stopped: terminal tool not submitted. tool_calls_used={}, text_only_no_terminal_turns={}, tool_call_limit={}, hard_ceiling={}",
            self.tool_calls_used,
            self.text_only_no_terminal_turns,
            self.tool_call_limit,
            self.hard_no_terminal_ceiling()
        )
    }

    pub(crate) async fn drain_notifications(&mut self) -> Vec<SystemNotification> {
        if let Some(background) = &self.background {
            background.flush_completions().await;
        }
        let messages = self.llm_messages();
        let rule_context = NotificationRuleContext {
            tool_calls_used: self.tool_calls_used,
            tool_call_limit: self.tool_call_limit,
            terminal_tools: &self.terminal_tools,
            terminal_submitted: false,
        };
        enqueue_notification_rules(
            &messages,
            &rule_context,
            &self.notification_rules,
            &mut self.notification_fired,
            &self.notifier,
        )
        .await;
        let notifications = self.notifier.drain().await;
        if !notifications.is_empty() {
            self.conversation_messages
                .push(AgentLoopMessage::UserMessage(notification_message(
                    &notifications,
                )));
        }
        notifications
    }

    pub(crate) async fn teardown_background(&self, reason: &str) {
        if let Some(teardown) = &self.background_teardown {
            teardown.teardown(reason).await;
        }
    }

    pub(crate) fn background(&self) -> Option<&BackgroundManagers> {
        self.background.as_ref()
    }

    fn hard_no_terminal_ceiling(&self) -> u32 {
        self.tool_call_limit.saturating_mul(3).saturating_add(1) / 2
    }

    fn llm_messages(&self) -> Vec<Message> {
        self.conversation_messages
            .iter()
            .filter_map(|message| match message {
                AgentLoopMessage::SystemPrompt(_) => None,
                AgentLoopMessage::UserMessage(message)
                | AgentLoopMessage::AssistantMessage(message) => Some(message.clone()),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AgentLoopRunServices {
    notifier: EngineNotificationQueue,
    background: Option<BackgroundManagers>,
    background_teardown: Option<BackgroundSessionTeardown>,
}

impl AgentLoopRunServices {
    pub(crate) fn inert() -> Self {
        Self {
            notifier: EngineNotificationQueue::new(),
            background: None,
            background_teardown: None,
        }
    }

    pub(crate) fn from_background(
        background: &BackgroundManagers,
        notifier: EngineNotificationQueue,
    ) -> Self {
        Self {
            notifier,
            background: Some(background.clone()),
            background_teardown: Some(background.session_teardown()),
        }
    }
}

fn notification_message(notifications: &[SystemNotification]) -> Message {
    Message {
        role: MessageRole::User,
        content: notifications
            .iter()
            .map(|notification| ContentBlock::SystemNotification {
                text: notification.message.clone(),
            })
            .collect(),
    }
}

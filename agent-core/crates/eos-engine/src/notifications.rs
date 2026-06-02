//! Declarative notification rules and the notification sink port.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::Message;
use eos_tools::ports::{
    AdvisorApproval, AdvisorPort, NotificationSink, Sealed, SystemNotification as ToolNotification,
};
use eos_tools::ToolError;
use tokio::sync::Mutex;

use crate::query::QueryContext;

/// A stream- and transcript-visible system notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemNotification {
    /// Notification text.
    pub text: String,
    /// Agent label.
    pub agent_name: String,
    /// Agent run id as a string.
    pub agent_run_id: String,
}

/// Closed set of engine-owned notification rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationRule {
    /// Nudge the model to submit a terminal tool.
    TerminalCallReminder,
    /// Tool-call budget threshold.
    ToolCallBudget {
        /// Human label, such as `75%`.
        label: &'static str,
        /// Threshold numerator.
        numerator: u32,
        /// Threshold denominator.
        denominator: u32,
    },
}

impl NotificationRule {
    /// Stable deduplication key.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::TerminalCallReminder => "terminal_call_reminder".to_owned(),
            Self::ToolCallBudget { label, .. } => {
                format!("tool_call_budget_{}_percent", label.trim_end_matches('%'))
            }
        }
    }

    /// Whether this rule fires only once per run.
    #[must_use]
    pub const fn fire_once(&self) -> bool {
        matches!(self, Self::ToolCallBudget { .. })
    }

    /// Whether this rule should fire for the current top-of-turn state.
    #[must_use]
    pub fn trigger(&self, _messages: &[Message], ctx: &QueryContext) -> bool {
        if ctx.terminal_result.as_ref().is_some_and(|r| r.is_terminal) {
            return false;
        }
        match self {
            Self::TerminalCallReminder => !ctx.terminal_tools.is_empty(),
            Self::ToolCallBudget {
                numerator,
                denominator,
                ..
            } => {
                if ctx.tool_call_limit == 0 || *denominator == 0 {
                    return false;
                }
                ctx.tool_calls_used.saturating_mul(*denominator)
                    >= ctx.tool_call_limit.saturating_mul(*numerator)
            }
        }
    }

    /// Render the reminder text.
    #[must_use]
    pub fn body(&self, ctx: &QueryContext) -> String {
        match self {
            Self::TerminalCallReminder => {
                let names = ctx
                    .terminal_tools
                    .iter()
                    .map(|name| format!("`{}`", name.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Remember to call a terminal tool when the task is complete. Available terminal tools: {names}.")
            }
            Self::ToolCallBudget { label, .. } => {
                let remaining = ctx.tool_call_limit.saturating_sub(ctx.tool_calls_used);
                format!(
                    "Tool-call budget reminder: {label} of the configured tool-call budget has been used; approximately {remaining} configured tool calls remain before terminal-submission pressure increases."
                )
            }
        }
    }
}

/// Default notification rules, deduped by name.
#[must_use]
pub fn make_default_notification_rules() -> Vec<NotificationRule> {
    let rules = [
        NotificationRule::ToolCallBudget {
            label: "75%",
            numerator: 3,
            denominator: 4,
        },
        NotificationRule::ToolCallBudget {
            label: "100%",
            numerator: 1,
            denominator: 1,
        },
        NotificationRule::ToolCallBudget {
            label: "125%",
            numerator: 5,
            denominator: 4,
        },
        NotificationRule::TerminalCallReminder,
    ];
    let mut seen = std::collections::BTreeSet::new();
    rules
        .into_iter()
        .filter(|rule| seen.insert(rule.name()))
        .collect()
}

/// Evaluate notification rules in list order.
#[must_use]
pub fn dispatch_rules(messages: &[Message], ctx: &mut QueryContext) -> Vec<SystemNotification> {
    let mut notifications = Vec::new();
    for rule in ctx.notification_rules.clone() {
        let name = rule.name();
        if rule.fire_once() && ctx.notification_fired.contains(&name) {
            continue;
        }
        if rule.trigger(messages, ctx) {
            if rule.fire_once() {
                ctx.notification_fired.insert(name);
            }
            notifications.push(SystemNotification {
                text: rule.body(ctx),
                agent_name: ctx.agent_name.clone(),
                agent_run_id: ctx.agent_run_id.to_string(),
            });
        }
    }
    notifications
}

/// Queue-backed notification sink for tools and hooks.
#[derive(Debug, Default, Clone)]
pub struct NotificationService {
    queue: Arc<Mutex<VecDeque<ToolNotification>>>,
}

impl NotificationService {
    /// Create an empty service.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain queued notifications.
    pub async fn drain(&self) -> Vec<ToolNotification> {
        self.queue.lock().await.drain(..).collect()
    }
}

impl Sealed for NotificationService {}

#[async_trait]
impl NotificationSink for NotificationService {
    async fn notify_system(&self, notification: ToolNotification) -> Result<(), ToolError> {
        self.queue.lock().await.push_back(notification);
        Ok(())
    }
}

/// Minimal advisor port implementation used until `eos-runtime` wires a helper
/// runner around the engine loop.
#[derive(Debug, Default, Clone)]
pub struct AdvisorService;

impl Sealed for AdvisorService {}

#[async_trait]
impl AdvisorPort for AdvisorService {
    async fn review(
        &self,
        tool_name: &str,
        _tool_payload: &eos_types::JsonObject,
    ) -> Result<String, ToolError> {
        Ok(format!(
            "Advisor runner is not wired for `{tool_name}` in this engine-only phase."
        ))
    }

    async fn approval_status(&self, _target_tool: &str) -> Result<AdvisorApproval, ToolError> {
        Ok(AdvisorApproval {
            approved: false,
            reason: Some("missing".to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use eos_tools::{ToolName, ToolRegistry, ToolResult};
    use eos_types::{AgentRunId, JsonObject};

    use super::*;
    use crate::test_support::metadata;

    fn ctx() -> QueryContext {
        QueryContext {
            tool_registry: Arc::new(ToolRegistry::new()),
            cwd: PathBuf::new(),
            model: "m".to_owned(),
            system_prompt: String::new(),
            max_tokens: 1,
            tool_call_limit: 4,
            agent_name: "root".to_owned(),
            agent_run_id: AgentRunId::new_v4(),
            task_id: None,
            tool_calls_used: 0,
            text_only_no_terminal_turns: 0,
            tool_metadata: metadata(),
            enable_background_tasks: true,
            terminal_tools: BTreeSet::from([ToolName::SubmitRootOutcome]),
            exit_reason: None,
            terminal_result: None,
            event_source: None,
            prompt_report: None,
            notification_rules: make_default_notification_rules(),
            notification_fired: BTreeSet::new(),
            notification_state: JsonObject::new(),
        }
    }

    #[test]
    fn notification_rules_fire_in_order_with_dedup() {
        let mut ctx = ctx();
        ctx.tool_calls_used = 3;
        let first = dispatch_rules(&[], &mut ctx);
        assert_eq!(first.len(), 2, "75% budget + terminal reminder");
        assert!(first[0].text.contains("75%"));
        assert!(first[1].text.contains("terminal tool"));

        let second = dispatch_rules(&[], &mut ctx);
        assert_eq!(second.len(), 1, "budget tier is fire-once");
        assert!(second[0].text.contains("terminal tool"));

        ctx.terminal_result = Some(ToolResult {
            output: "done".to_owned(),
            is_error: false,
            metadata: JsonObject::new(),
            is_terminal: true,
        });
        assert!(dispatch_rules(&[], &mut ctx).is_empty());
    }

    #[tokio::test]
    async fn notification_service_queues_and_drains() {
        let service = NotificationService::new();
        service
            .notify_system(ToolNotification {
                event: "evt".to_owned(),
                message: "body".to_owned(),
            })
            .await
            .expect("notify");
        let drained = service.drain().await;
        assert_eq!(drained.len(), 1);
        assert!(service.drain().await.is_empty());
    }
}

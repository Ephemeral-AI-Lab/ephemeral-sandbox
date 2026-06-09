//! The notification sink, the [`NotificationRule`] abstraction, and rule
//! evaluation.
//!
//! Each concrete rule lives in its own module — the terminal-submit reminder in
//! [`terminal_reminder`] and the tool-call budget tiers in [`tool_budget`].
//! This file owns the [`NotificationRule`] trait, the default rule set, the
//! loop-facing [`enqueue_notification_rules`] (anchor D4: every notification is
//! a sink producer), and the queue-backed [`EngineNotificationQueue`].

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::Message;
use eos_tool::{ToolError, ToolKey};
use tokio::sync::Mutex;

mod rules {
    //! Concrete [`NotificationRule`](crate::notifications::NotificationRule)
    //! implementations, one per file: the terminal-submit reminder
    //! ([`terminal_reminder`]) and the tool-call budget tiers ([`tool_budget`]).

    mod terminal_reminder {
        //! The terminal-submit reminder rule (anchor §6.2).
        //!
        //! Nudges the model to call a terminal tool when its most recent assistant turn
        //! was a *text return* — a `Text` block and no `ToolUse`. A reasoning-only turn
        //! (no `Text` block) does not nudge; a tool-use turn is making progress, so it
        //! does not nudge either.

        use eos_llm_client::{ContentBlock, Message, MessageRole};

        use crate::notifications::{budget_figures, NotificationRule, NotificationRuleContext};

        /// Reminds the model to submit a terminal tool after a bare text return.
        #[derive(Debug, Clone, Copy, Default)]
        pub struct TerminalCallReminder;

        impl NotificationRule for TerminalCallReminder {
            fn name(&self) -> String {
                "terminal_call_reminder".to_owned()
            }

            fn fire_once(&self) -> bool {
                false
            }

            fn trigger(&self, messages: &[Message], ctx: &NotificationRuleContext<'_>) -> bool {
                !ctx.terminal_tools.is_empty() && last_assistant_was_text_return(messages)
            }

            fn body(&self, ctx: &NotificationRuleContext<'_>) -> String {
                let (used, limit, ceiling, turns_remaining) = budget_figures(ctx);
                let mut names: Vec<&str> = ctx
                    .terminal_tools
                    .iter()
                    .map(eos_tool::ToolKey::as_str)
                    .collect();
                names.sort_unstable();
                let names = names.join(", ");
                format!(
                    "You have not submitted a terminal tool. Deliver your result by \
                      calling one of: {names}. Budget: {used}/{limit} tool calls used; \
                      the run will fail at {ceiling} tool calls ({turns_remaining} remaining)."
                )
            }
        }

        /// Whether the most recent assistant turn was a text return: a
        /// [`ContentBlock::Text`] and no [`ContentBlock::ToolUse`]. A reasoning-only
        /// turn (no `Text`) returns `false`.
        fn last_assistant_was_text_return(messages: &[Message]) -> bool {
            let Some(message) = messages
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
            else {
                return false;
            };
            let has_text = message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { .. }));
            let has_tool_use = message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
            has_text && !has_tool_use
        }
    }
    mod tool_budget {
        //! The tool-call budget reminder rule (anchor §6.1).
        //!
        //! Fires once per tier (75/100/125% of the planned `tool_call_limit`) to warn
        //! the model that it is approaching the hard ceiling at which the run fails.

        use eos_llm_client::Message;

        use crate::notifications::{budget_figures, NotificationRule, NotificationRuleContext};

        /// A single tool-call budget tier (e.g. `75%`), firing once when
        /// `tool_calls_used * denominator >= tool_call_limit * numerator`.
        #[derive(Debug, Clone)]
        pub struct ToolCallBudget {
            /// Human label, such as `75%`.
            label: &'static str,
            /// Threshold numerator.
            numerator: u32,
            /// Threshold denominator.
            denominator: u32,
        }

        impl ToolCallBudget {
            /// Construct one budget tier.
            #[must_use]
            pub const fn new(label: &'static str, numerator: u32, denominator: u32) -> Self {
                Self {
                    label,
                    numerator,
                    denominator,
                }
            }
        }

        impl NotificationRule for ToolCallBudget {
            fn name(&self) -> String {
                format!(
                    "tool_call_budget_{}_percent",
                    self.label.trim_end_matches('%')
                )
            }

            fn fire_once(&self) -> bool {
                true
            }

            fn trigger(&self, _messages: &[Message], ctx: &NotificationRuleContext<'_>) -> bool {
                if ctx.tool_call_limit == 0 || self.denominator == 0 {
                    return false;
                }
                ctx.tool_calls_used.saturating_mul(self.denominator)
                    >= ctx.tool_call_limit.saturating_mul(self.numerator)
            }

            fn body(&self, ctx: &NotificationRuleContext<'_>) -> String {
                let (used, limit, ceiling, turns_remaining) = budget_figures(ctx);
                let label = self.label;
                format!(
                    "Tool-call budget warning: {label} of the planned budget has been \
                     used ({used}/{limit} tool calls). Submit a terminal tool as soon \
                     as the work is complete; the run will fail at {ceiling} tool calls \
                     ({turns_remaining} remaining)."
                )
            }
        }
    }

    pub use terminal_reminder::TerminalCallReminder;
    pub use tool_budget::ToolCallBudget;
}

pub use rules::{TerminalCallReminder, ToolCallBudget};

/// A system notification the engine surfaces to the model transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemNotification {
    /// The notification event key.
    pub event: String,
    /// Free-text body.
    pub message: String,
}

/// The engine notification sink used by notification rules and background
/// completion emitters.
#[async_trait]
pub trait NotificationSink: Send + Sync {
    /// Surface one system notification.
    async fn notify_system(&self, notification: SystemNotification) -> Result<(), ToolError>;
}

/// One engine-owned notification rule. Concrete rules live in their own modules
/// and are registered in [`make_default_notification_rules`].
pub trait NotificationRule: Send + Sync {
    /// Stable deduplication / sink-event key.
    fn name(&self) -> String;

    /// Whether the rule fires at most once per run (latched in
    /// `notification_fired`).
    fn fire_once(&self) -> bool;

    /// Whether the rule should fire for the current top-of-turn state.
    fn trigger(&self, messages: &[Message], ctx: &NotificationRuleContext<'_>) -> bool;

    /// Render the notification body.
    fn body(&self, ctx: &NotificationRuleContext<'_>) -> String;
}

/// Read-only loop facts needed by notification rules.
#[derive(Debug, Clone, Copy)]
pub struct NotificationRuleContext<'a> {
    /// Counted model-requested tool calls.
    pub tool_calls_used: u32,
    /// Configured tool-call limit.
    pub tool_call_limit: u32,
    /// Terminal tools visible to this loop.
    pub terminal_tools: &'a BTreeSet<ToolKey>,
    /// Whether a terminal submission has already ended the run.
    pub terminal_submitted: bool,
}

/// Shared budget arithmetic for the rule bodies: `(used, limit, ceiling,
/// turns_remaining)`. The run fails at `ceiling = ceil(1.5 * limit)` tool
/// calls; `turns_remaining` is derived from `tool_calls_used` alone (the
/// hard-ceiling gate itself uses the call+text-turn sum — anchor §6.5).
pub(crate) fn budget_figures(ctx: &NotificationRuleContext<'_>) -> (u32, u32, u32, u32) {
    let used = ctx.tool_calls_used;
    let limit = ctx.tool_call_limit;
    let ceiling = limit.saturating_mul(3).div_ceil(2);
    let turns_remaining = ceiling.saturating_sub(used);
    (used, limit, ceiling, turns_remaining)
}

/// Default notification rules, deduped by name: the three tool-call budget tiers
/// (75/100/125%) plus the terminal-submit reminder.
#[must_use]
pub fn make_default_notification_rules() -> Vec<Arc<dyn NotificationRule>> {
    let rules: Vec<Arc<dyn NotificationRule>> = vec![
        Arc::new(ToolCallBudget::new("75%", 3, 4)),
        Arc::new(ToolCallBudget::new("100%", 1, 1)),
        Arc::new(ToolCallBudget::new("125%", 5, 4)),
        Arc::new(TerminalCallReminder),
    ];
    let mut seen = std::collections::BTreeSet::new();
    rules
        .into_iter()
        .filter(|rule| seen.insert(rule.name()))
        .collect()
}

/// Evaluate notification rules in list order and enqueue the firing ones onto
/// the sink. A submitted terminal silences all rules; fire-once budget tiers are
/// latched in `ctx.notification_fired`; the loop is the sole sink consumer.
pub async fn enqueue_notification_rules(
    messages: &[Message],
    ctx: &NotificationRuleContext<'_>,
    rules: &[Arc<dyn NotificationRule>],
    notification_fired: &mut BTreeSet<String>,
    sink: &dyn NotificationSink,
) {
    if ctx.terminal_submitted {
        return;
    }
    for rule in rules {
        let name = rule.name();
        if rule.fire_once() && notification_fired.contains(&name) {
            continue;
        }
        if rule.trigger(messages, ctx) {
            if rule.fire_once() {
                notification_fired.insert(name.clone());
            }
            let _ = sink
                .notify_system(SystemNotification {
                    event: name,
                    message: rule.body(ctx),
                })
                .await;
        }
    }
}

/// Queue-backed notification sink for tools and hooks.
#[derive(Debug, Default, Clone)]
pub struct EngineNotificationQueue {
    queue: Arc<Mutex<VecDeque<SystemNotification>>>,
}

impl EngineNotificationQueue {
    /// Create an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain queued notifications.
    pub async fn drain(&self) -> Vec<SystemNotification> {
        self.queue.lock().await.drain(..).collect()
    }
}

#[async_trait]
impl NotificationSink for EngineNotificationQueue {
    async fn notify_system(&self, notification: SystemNotification) -> Result<(), ToolError> {
        self.queue.lock().await.push_back(notification);
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/notifications/mod.rs"]
mod tests;

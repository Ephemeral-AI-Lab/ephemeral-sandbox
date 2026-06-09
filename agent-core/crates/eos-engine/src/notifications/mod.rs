//! The notification sink, the [`NotificationRule`] abstraction, and rule
//! evaluation.
//!
//! Each concrete rule lives in its own module — the terminal-submit reminder in
//! [`terminal_reminder`] and the tool-call budget tiers in [`tool_budget`].
//! This file owns the [`NotificationRule`] trait, the default rule set, the
//! loop-facing [`enqueue_notification_rules`] (anchor D4: every notification is
//! a sink producer), and the queue-backed [`NotificationService`].

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::Message;
use eos_tool::{ToolError, ToolKey};
use tokio::sync::Mutex;

mod rules;

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
pub struct NotificationService {
    queue: Arc<Mutex<VecDeque<SystemNotification>>>,
}

impl NotificationService {
    /// Create an empty service.
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
impl NotificationSink for NotificationService {
    async fn notify_system(&self, notification: SystemNotification) -> Result<(), ToolError> {
        self.queue.lock().await.push_back(notification);
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/notifications/mod.rs"]
mod tests;

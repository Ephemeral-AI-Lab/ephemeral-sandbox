//! The notification sink, the [`NotificationRule`] abstraction, and rule
//! evaluation.
//!
//! Each concrete rule lives in its own module — the terminal-submit reminder in
//! [`terminal_reminder`] and the tool-call budget tiers in [`tool_budget`].
//! This file owns the [`NotificationRule`] trait, the default rule set, the
//! loop-facing [`enqueue_notification_rules`] (anchor D4: every notification is
//! a sink producer), and the queue-backed [`NotificationService`].

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::Message;
use eos_tool_ports::{NotificationSink, Sealed, SystemNotification as ToolNotification, ToolError};
use tokio::sync::Mutex;

use crate::query::QueryContext;

mod rules;

pub use rules::{TerminalCallReminder, ToolCallBudget};

/// One engine-owned notification rule. Concrete rules live in their own modules
/// and are registered in [`make_default_notification_rules`].
pub trait NotificationRule: Send + Sync {
    /// Stable deduplication / sink-event key.
    fn name(&self) -> String;

    /// Whether the rule fires at most once per run (latched in
    /// `ctx.notification_fired`).
    fn fire_once(&self) -> bool;

    /// Whether the rule should fire for the current top-of-turn state. The
    /// already-submitted-terminal short-circuit is handled by
    /// [`enqueue_notification_rules`], so rules need not re-check it.
    fn trigger(&self, messages: &[Message], ctx: &QueryContext) -> bool;

    /// Render the notification body.
    fn body(&self, ctx: &QueryContext) -> String;
}

/// Shared budget arithmetic for the rule bodies: `(used, limit, ceiling,
/// turns_remaining)`. The run fails at `ceiling = ceil(1.5 * limit)` tool
/// calls; `turns_remaining` is derived from `tool_calls_used` alone (the
/// hard-ceiling gate itself uses the call+text-turn sum — anchor §6.5).
pub(crate) fn budget_figures(ctx: &QueryContext) -> (u32, u32, u32, u32) {
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
    ctx: &mut QueryContext,
    sink: &dyn NotificationSink,
) {
    if ctx
        .submission_outcome
        .as_ref()
        .is_some_and(|result| result.is_terminal)
    {
        return;
    }
    for rule in ctx.notification_rules.clone() {
        let name = rule.name();
        if rule.fire_once() && ctx.notification_was_fired(&name) {
            continue;
        }
        if rule.trigger(messages, ctx) {
            if rule.fire_once() {
                ctx.mark_notification_fired(name.clone());
            }
            let _ = sink
                .notify_system(ToolNotification {
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

#[cfg(test)]
#[path = "../../tests/notifications/mod.rs"]
mod tests;

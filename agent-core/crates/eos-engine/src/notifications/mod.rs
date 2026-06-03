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
use eos_tools::ports::{
    AdvisorApproval, AdvisorPort, NotificationSink, Sealed, SystemNotification as ToolNotification,
};
use eos_tools::ToolError;
use tokio::sync::Mutex;

use crate::query::QueryContext;

mod rules;

pub use rules::{TerminalCallReminder, ToolCallBudget};

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
        .terminal_result
        .as_ref()
        .is_some_and(|result| result.is_terminal)
    {
        return;
    }
    for rule in ctx.notification_rules.clone() {
        let name = rule.name();
        if rule.fire_once() && ctx.notification_fired.contains(&name) {
            continue;
        }
        if rule.trigger(messages, ctx) {
            if rule.fire_once() {
                ctx.notification_fired.insert(name.clone());
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

    use eos_llm_client::{ContentBlock, MessageRole};
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
            notifier: NotificationService::new(),
            run_handles: None,
        }
    }

    fn text_turn(text: &str) -> [Message; 1] {
        [Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
        }]
    }

    fn reasoning_turn(text: &str) -> [Message; 1] {
        [Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: text.to_owned(),
            }],
        }]
    }

    fn tool_turn() -> [Message; 1] {
        [Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                tool_use_id: "toolu-1".parse().expect("tool use id"),
                name: "read_file".to_owned(),
                input: JsonObject::new(),
            }],
        }]
    }

    /// Enqueue the firing rules onto the context notifier and drain them — the
    /// loop-top sequence in miniature.
    async fn fire_rules(messages: &[Message], ctx: &mut QueryContext) -> Vec<ToolNotification> {
        let notifier = ctx.notifier.clone();
        enqueue_notification_rules(messages, ctx, &notifier).await;
        notifier.drain().await
    }

    #[tokio::test]
    async fn notification_rules_fire_in_order_with_dedup() {
        let mut ctx = ctx();
        ctx.tool_calls_used = 3; // tool_call_limit = 4 -> the 75% tier fires
        let turn = text_turn("here is my answer");
        let first = fire_rules(&turn, &mut ctx).await;
        assert_eq!(first.len(), 2, "75% budget + terminal reminder");
        assert!(first[0].message.contains("75%"), "budget tier first");
        assert!(first[1].message.contains("terminal tool"));

        let second = fire_rules(&turn, &mut ctx).await;
        assert_eq!(second.len(), 1, "budget tier is fire-once");
        assert!(second[0].message.contains("terminal tool"));

        ctx.terminal_result = Some(ToolResult {
            output: "done".to_owned(),
            is_error: false,
            metadata: JsonObject::new(),
            is_terminal: true,
        });
        assert!(fire_rules(&turn, &mut ctx).await.is_empty());
    }

    #[tokio::test]
    async fn terminal_reminder_fires_on_text_return_and_reports_budget() {
        let mut ctx = ctx(); // tool_call_limit = 4
        ctx.tool_calls_used = 2; // below the first budget tier; only the reminder can fire

        // User-only transcript -> no assistant text return -> no terminal reminder.
        assert!(fire_rules(&[Message::from_user_text("hi")], &mut ctx)
            .await
            .is_empty());

        // A text-return assistant turn nudges, with the ceil(1.5*limit) ceiling.
        let fired = fire_rules(&text_turn("done"), &mut ctx).await;
        assert_eq!(fired.len(), 1);
        let body = &fired[0].message;
        assert!(body.contains("You have not submitted a terminal tool"));
        assert!(body.contains("2/4 tool calls used"));
        assert!(body.contains("the run will fail at 6 tool calls (4 remaining)"));
    }

    #[tokio::test]
    async fn reasoning_only_does_not_nudge_but_text_only_does() {
        let mut ctx = ctx();
        ctx.tool_calls_used = 2; // below every budget tier; only the nudge can fire

        // Reasoning-only turn (no Text block) -> no nudge.
        assert!(fire_rules(&reasoning_turn("thinking"), &mut ctx)
            .await
            .is_empty());
        // Tool-use turn (making progress) -> no nudge.
        assert!(fire_rules(&tool_turn(), &mut ctx).await.is_empty());
        // Text-only turn -> nudge.
        assert_eq!(fire_rules(&text_turn("answer"), &mut ctx).await.len(), 1);
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

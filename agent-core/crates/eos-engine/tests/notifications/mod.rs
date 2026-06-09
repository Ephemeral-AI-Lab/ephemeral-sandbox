#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use eos_llm_client::{ContentBlock, MessageRole};
use eos_tool::{ToolKey, ToolName};
use eos_types::JsonObject;

use super::*;

struct RuleFixture {
    rules: Vec<Arc<dyn NotificationRule>>,
    fired: BTreeSet<String>,
    terminal_tools: BTreeSet<ToolKey>,
    tool_calls_used: u32,
    tool_call_limit: u32,
    terminal_submitted: bool,
    notifier: NotificationService,
}

impl RuleFixture {
    fn new() -> Self {
        Self {
            rules: make_default_notification_rules(),
            fired: BTreeSet::new(),
            terminal_tools: BTreeSet::from([ToolKey::from(ToolName::SubmitRootOutcome)]),
            tool_calls_used: 0,
            tool_call_limit: 4,
            terminal_submitted: false,
            notifier: NotificationService::new(),
        }
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
async fn fire_rules(messages: &[Message], fixture: &mut RuleFixture) -> Vec<SystemNotification> {
    let terminal_tools = fixture.terminal_tools.clone();
    let context = NotificationRuleContext {
        tool_calls_used: fixture.tool_calls_used,
        tool_call_limit: fixture.tool_call_limit,
        terminal_tools: &terminal_tools,
        terminal_submitted: fixture.terminal_submitted,
    };
    enqueue_notification_rules(
        messages,
        &context,
        &fixture.rules,
        &mut fixture.fired,
        &fixture.notifier,
    )
    .await;
    fixture.notifier.drain().await
}

#[tokio::test]
async fn notification_rules_fire_in_order_with_dedup() {
    let mut fixture = RuleFixture::new();
    fixture.tool_calls_used = 3; // tool_call_limit = 4 -> the 75% tier fires
    let turn = text_turn("here is my answer");
    let first = fire_rules(&turn, &mut fixture).await;
    assert_eq!(first.len(), 2, "75% budget + terminal reminder");
    assert!(first[0].message.contains("75%"), "budget tier first");
    assert!(first[1].message.contains("terminal tool"));

    let second = fire_rules(&turn, &mut fixture).await;
    assert_eq!(second.len(), 1, "budget tier is fire-once");
    assert!(second[0].message.contains("terminal tool"));

    fixture.terminal_submitted = true;
    assert!(fire_rules(&turn, &mut fixture).await.is_empty());
}

#[tokio::test]
async fn terminal_reminder_fires_on_text_return_and_reports_budget() {
    let mut fixture = RuleFixture::new(); // tool_call_limit = 4
    fixture.tool_calls_used = 2; // below the first budget tier; only the reminder can fire

    // User-only transcript -> no assistant text return -> no terminal reminder.
    assert!(fire_rules(&[Message::from_user_text("hi")], &mut fixture)
        .await
        .is_empty());

    // A text-return assistant turn nudges, with the ceil(1.5*limit) ceiling.
    let fired = fire_rules(&text_turn("done"), &mut fixture).await;
    assert_eq!(fired.len(), 1);
    let body = &fired[0].message;
    assert!(body.contains("You have not submitted a terminal tool"));
    assert!(body.contains("2/4 tool calls used"));
    assert!(body.contains("the run will fail at 6 tool calls (4 remaining)"));
}

#[tokio::test]
async fn reasoning_only_does_not_nudge_but_text_only_does() {
    let mut fixture = RuleFixture::new();
    fixture.tool_calls_used = 2; // below every budget tier; only the nudge can fire

    // Reasoning-only turn (no Text block) -> no nudge.
    assert!(fire_rules(&reasoning_turn("thinking"), &mut fixture)
        .await
        .is_empty());
    // Tool-use turn (making progress) -> no nudge.
    assert!(fire_rules(&tool_turn(), &mut fixture).await.is_empty());
    // Text-only turn -> nudge.
    assert_eq!(
        fire_rules(&text_turn("answer"), &mut fixture).await.len(),
        1
    );
}

#[tokio::test]
async fn notification_service_queues_and_drains() {
    let service = NotificationService::new();
    service
        .notify_system(SystemNotification {
            event: "evt".to_owned(),
            message: "body".to_owned(),
        })
        .await
        .expect("notify");
    let drained = service.drain().await;
    assert_eq!(drained.len(), 1);
    assert!(service.drain().await.is_empty());
}

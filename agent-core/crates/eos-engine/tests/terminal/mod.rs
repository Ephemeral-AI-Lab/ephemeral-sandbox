//! Terminal-enforcement tests (`TESTING_SPEC` §5.2 "terminal enforcement",
//! checkpoint: targeted drop at the synthetic-failure event).
//!
//! An **integration** target: it drives the real `run_query` loop with
//! `eos-testkit`'s `ScriptedSource`, whose `EventSource` impl `eos-engine` owns —
//! so it is only consumable where `eos-engine` is an external dependency (the
//! dev-dep two-instance rule). Both tests pull the stream to the synthetic
//! ceiling-failure event, then `drop(stream)` and inspect `ctx` **without
//! reaching a terminal/`ToolStop`** — the Layer-A non-closure checkpoint
//! (`TESTING_SPEC` §4 / AC6).
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use eos_engine::{
    run_query, EventSource, NotificationService, QueryContext, QueryExitReason, StreamEvent,
};
use eos_llm_client::{ContentBlock, Message};
use eos_testkit::{metadata, run_until, text_turn, ScriptedSource};
use eos_tools::{NotificationSink, SystemNotification, ToolName, ToolRegistry};
use eos_types::AgentRunId;

/// A query context with a tight `tool_call_limit` (hard ceiling = 3) so a few
/// text-only turns trip the terminal-not-submitted guard.
fn ctx(source: Arc<dyn EventSource>) -> QueryContext {
    QueryContext {
        tool_registry: Arc::new(ToolRegistry::new()),
        cwd: PathBuf::new(),
        model: "m".to_owned(),
        system_prompt: String::new(),
        max_tokens: 1,
        reasoning_effort: None,
        tool_call_limit: 2,
        agent_name: "root".to_owned(),
        agent_run_id: AgentRunId::new_v4(),
        task_id: None,
        tool_calls_used: 0,
        text_only_no_terminal_turns: 0,
        tool_metadata: metadata(),
        terminal_tools: BTreeSet::from([ToolName::SubmitRootOutcome]),
        exit_reason: None,
        terminal_result: None,
        event_source: Some(source),
        prompt_report: None,
        notification_rules: Vec::new(),
        notification_fired: BTreeSet::new(),
        notifier: NotificationService::new(),
        audit: None,
        run_handles: None,
    }
}

fn is_ceiling_failure(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::ToolExecutionCompleted { tool_name, is_error, .. }
            if tool_name.is_empty() && *is_error
    )
}

#[tokio::test]
async fn hard_ceiling_exit_terminal_not_submitted() {
    let source = Arc::new(ScriptedSource::new(vec![
        text_turn("one"),
        text_turn("two"),
        text_turn("three"),
    ]));
    let mut ctx = ctx(source);
    let mut messages = vec![Message::from_user_text("start")];

    let mut stream = run_query(&mut ctx, &mut messages);
    // Pull to the synthetic ceiling-failure event, then drop the stream — the
    // non-closure checkpoint (`TESTING_SPEC` §4): no `ToolStop`, just the guard.
    let events = run_until(&mut stream, is_ceiling_failure).await;
    drop(stream);

    let output = match events.last() {
        Some(StreamEvent::ToolExecutionCompleted { output, .. }) => output.clone(),
        other => panic!("expected the ceiling-failure event last, got {other:?}"),
    };
    assert_eq!(
        output,
        "Agent stopped: terminal tool not submitted. tool_calls_used=0, text_only_no_terminal_turns=3, tool_call_limit=2, hard_ceiling=3"
    );
    assert_eq!(ctx.exit_reason, Some(QueryExitReason::TerminalNotSubmitted));
}

#[tokio::test]
async fn terminal_not_submitted_drains_queued_notifications_before_exit() {
    let source = Arc::new(ScriptedSource::new(Vec::new()));
    let mut ctx = ctx(source);
    // Pre-trip the ceiling so the first loop pass exits — but only after draining
    // the queued background-completion notification.
    ctx.tool_calls_used = 3;
    ctx.notifier
        .notify_system(SystemNotification {
            event: "cmd_1".to_owned(),
            message: "[BACKGROUND COMPLETED] cmd_1".to_owned(),
        })
        .await
        .expect("notification queued");
    let mut messages = vec![Message::from_user_text("start")];

    let mut stream = run_query(&mut ctx, &mut messages);
    let events = run_until(&mut stream, is_ceiling_failure).await;
    drop(stream);

    let saw_notification = events.iter().any(|event| {
        matches!(event, StreamEvent::SystemNotification { text, .. }
            if text.contains("[BACKGROUND COMPLETED] cmd_1"))
    });
    assert!(saw_notification, "queued completion must be streamed");
    assert!(
        is_ceiling_failure(events.last().expect("a failure event")),
        "hard ceiling still emits the failure event"
    );
    assert!(
        messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::SystemNotification { text }
                    if text.contains("[BACKGROUND COMPLETED] cmd_1"))
            })
        }),
        "queued completion must be appended to the transcript before exit"
    );
}

//! Query loop.

use std::collections::BTreeSet;
use std::pin::Pin;

use async_stream::try_stream;
use eos_llm_client::{ContentBlock, Message, MessageRole, UsageSnapshot};
use eos_tools::SystemNotification;
use eos_types::{JsonObject, ToolUseId};
use futures::{Stream, StreamExt};

use crate::events::{stamp_identity, StreamEvent};
use crate::notifications::enqueue_notification_rules;
use crate::query::{build_query_run_request, QueryContext, QueryExitReason};
use crate::tool_call::{dispatch_assistant_tools, ToolUseRequest};
use crate::EngineError;

/// Query-loop output stream.
pub type QueryStream<'a> = Pin<
    Box<dyn Stream<Item = Result<(StreamEvent, Option<UsageSnapshot>), EngineError>> + Send + 'a>,
>;

/// Whether the terminal non-submission ceiling has been reached.
#[must_use]
pub fn terminal_submission_failed(ctx: &QueryContext) -> bool {
    let ceiling = ctx.tool_call_limit.saturating_mul(3).saturating_add(1) / 2;
    ctx.tool_calls_used
        .saturating_add(ctx.text_only_no_terminal_turns)
        >= ceiling
}

fn terminal_not_submitted_message(ctx: &QueryContext) -> String {
    format!(
        "The agent used {} tool calls/text-only turns without submitting a terminal tool. Submit one of the terminal tools to finish the run.",
        ctx.tool_calls_used
            .saturating_add(ctx.text_only_no_terminal_turns)
    )
}

fn synthetic_tool_use_id() -> Result<ToolUseId, EngineError> {
    "terminal_not_submitted".parse().map_err(EngineError::from)
}

fn tool_uses_from_message(message: &Message) -> Vec<ToolUseRequest> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                tool_use_id,
                name,
                input,
            } => Some(ToolUseRequest {
                tool_use_id: tool_use_id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn append_notifications(messages: &mut Vec<Message>, notifications: &[SystemNotification]) {
    if notifications.is_empty() {
        return;
    }
    messages.push(Message {
        role: MessageRole::User,
        content: notifications
            .iter()
            .map(|notification| ContentBlock::SystemNotification {
                text: notification.message.clone(),
            })
            .collect(),
    });
}

async fn collect_notifications(
    ctx: &mut QueryContext,
    messages: &mut Vec<Message>,
) -> Vec<SystemNotification> {
    let notifier = ctx.notifier.clone();
    enqueue_notification_rules(messages, ctx, &notifier).await;
    notifier.drain().await
}

fn notification_event(ctx: &QueryContext, notification: &SystemNotification) -> StreamEvent {
    StreamEvent::SystemNotification {
        agent_name: ctx.agent_name.clone(),
        agent_run_id: Some(ctx.agent_run_id.clone()),
        text: notification.message.clone(),
    }
}

fn tool_result_message(tool_results: Vec<ContentBlock>) -> Message {
    Message {
        role: MessageRole::User,
        content: tool_results,
    }
}

fn terminal_not_submitted_event(ctx: &mut QueryContext) -> Result<StreamEvent, EngineError> {
    ctx.exit_reason = Some(QueryExitReason::TerminalNotSubmitted);
    Ok(StreamEvent::ToolExecutionCompleted {
        agent_name: ctx.agent_name.clone(),
        agent_run_id: Some(ctx.agent_run_id.clone()),
        tool_name: String::new(),
        output: terminal_not_submitted_message(ctx),
        is_error: true,
        tool_use_id: synthetic_tool_use_id()?,
        metadata: JsonObject::new(),
        is_terminal: false,
    })
}

/// Run the query loop.
#[must_use]
pub fn run_query<'a>(ctx: &'a mut QueryContext, messages: &'a mut Vec<Message>) -> QueryStream<'a> {
    Box::pin(try_stream! {
        loop {
            if terminal_submission_failed(ctx) {
                let notifications = collect_notifications(ctx, messages).await;
                for notification in &notifications {
                    yield (notification_event(ctx, notification), None);
                }
                append_notifications(messages, &notifications);
                yield (terminal_not_submitted_event(ctx)?, None);
                break;
            }

            let notifications = collect_notifications(ctx, messages).await;
            for notification in &notifications {
                yield (notification_event(ctx, notification), None);
            }
            append_notifications(messages, &notifications);

            let run_request = build_query_run_request(ctx, messages).await;
            if let Some(recorder) = &ctx.prompt_report {
                recorder
                    .record_llm_request(
                        run_request.prompt_report_seq,
                        &ctx.system_prompt,
                        &run_request.request.messages,
                        &run_request.request.tools,
                    )
                    .await?;
            }
            let source = ctx.event_source.clone().ok_or(EngineError::MissingEventSource)?;
            let mut stream = source.stream(&run_request.request).await?;
            let mut final_message: Option<Message> = None;
            let mut final_usage: Option<UsageSnapshot> = None;
            let mut streamed_tool_use_ids = BTreeSet::new();

            while let Some(item) = stream.next().await {
                let event = stamp_identity(item?, &ctx.agent_name, &ctx.agent_run_id);
                match &event {
                    StreamEvent::ToolUseDelta { tool_use_id, .. } => {
                        if streamed_tool_use_ids.insert(tool_use_id.clone()) {
                            ctx.tool_calls_used = ctx.tool_calls_used.saturating_add(1);
                        }
                    }
                    StreamEvent::AssistantMessageComplete { payload, .. } => {
                        final_usage = Some(payload.usage);
                        final_message = Some(payload.message.clone());
                    }
                    _ => {}
                }
                let usage = match &event {
                    StreamEvent::AssistantMessageComplete { payload, .. } => Some(payload.usage),
                    _ => None,
                };
                yield (event, usage);
            }

            let message = match final_message {
                Some(message) => message,
                None => Err(EngineError::Internal(
                    "provider stream ended without assistant completion".to_owned(),
                ))?,
            };
            let usage = final_usage.unwrap_or_default();
            if let Some(recorder) = &ctx.prompt_report {
                recorder
                    .record_assistant(run_request.prompt_report_seq, &message, usage)
                    .await?;
            }
            let tool_uses = tool_uses_from_message(&message);
            for call in &tool_uses {
                if !streamed_tool_use_ids.contains(&call.tool_use_id) {
                    ctx.tool_calls_used = ctx.tool_calls_used.saturating_add(1);
                }
            }

            messages.push(message.clone());
            if tool_uses.is_empty() {
                ctx.text_only_no_terminal_turns =
                    ctx.text_only_no_terminal_turns.saturating_add(1);
                if terminal_submission_failed(ctx) {
                    let notifications = collect_notifications(ctx, messages).await;
                    for notification in &notifications {
                        yield (notification_event(ctx, notification), None);
                    }
                    append_notifications(messages, &notifications);
                    yield (terminal_not_submitted_event(ctx)?, None);
                    break;
                }
                continue;
            }

            let outcome = dispatch_assistant_tools(ctx, &tool_uses, messages).await?;
            for event in outcome.events {
                let stamped = stamp_identity(event, &ctx.agent_name, &ctx.agent_run_id);
                yield (stamped, None);
            }
            if let Some(recorder) = &ctx.prompt_report {
                recorder
                    .record_tool_results(run_request.prompt_report_seq, &outcome.tool_results)
                    .await?;
            }
            messages.push(tool_result_message(outcome.tool_results));

            if outcome
                .terminal_result
                .as_ref()
                .is_some_and(|result| result.is_terminal)
            {
                ctx.exit_reason = Some(QueryExitReason::ToolStop);
                let notifications = collect_notifications(ctx, messages).await;
                for notification in &notifications {
                    yield (notification_event(ctx, notification), None);
                }
                append_notifications(messages, &notifications);
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_llm_client::{ContentBlock, Message, MessageRole, UsageSnapshot};
    use eos_tools::{NotificationSink, SystemNotification, ToolName, ToolRegistry};
    use eos_types::{AgentRunId, JsonObject};
    use futures::StreamExt;

    use super::*;
    use crate::query::{EngineStream, EventSource};
    use crate::test_support::metadata;
    use crate::AssistantMessageComplete;

    #[derive(Debug)]
    struct ScriptedSource {
        turns: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
    }

    #[async_trait]
    impl EventSource for ScriptedSource {
        async fn stream(
            &self,
            _request: &eos_llm_client::LlmRequest,
        ) -> Result<EngineStream, EngineError> {
            let mut turns = self.turns.lock().await;
            let events = if turns.is_empty() {
                Vec::new()
            } else {
                turns.remove(0)
            };
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }
    }

    fn complete(text: &str) -> StreamEvent {
        StreamEvent::AssistantMessageComplete {
            agent_name: String::new(),
            agent_run_id: None,
            payload: Box::new(AssistantMessageComplete {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: text.to_owned(),
                    }],
                },
                usage: UsageSnapshot::default(),
                stop_reason: None,
            }),
        }
    }

    fn ctx(source: Arc<dyn EventSource>) -> QueryContext {
        QueryContext {
            tool_registry: Arc::new(ToolRegistry::new()),
            cwd: PathBuf::new(),
            model: "m".to_owned(),
            system_prompt: String::new(),
            max_tokens: 1,
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
            notification_state: JsonObject::new(),
            notifier: crate::NotificationService::new(),
            audit: None,
            run_handles: None,
        }
    }

    #[tokio::test]
    async fn hard_ceiling_exit_terminal_not_submitted() {
        let source = Arc::new(ScriptedSource {
            turns: tokio::sync::Mutex::new(vec![
                vec![complete("one")],
                vec![complete("two")],
                vec![complete("three")],
            ]),
        });
        let mut ctx = ctx(source);
        let mut messages = vec![Message::from_user_text("start")];
        let mut stream = run_query(&mut ctx, &mut messages);
        let mut failure = None;
        while let Some(item) = stream.next().await {
            let (event, _) = item.expect("stream item");
            if let StreamEvent::ToolExecutionCompleted {
                tool_name,
                is_error,
                output,
                ..
            } = event
            {
                if tool_name.is_empty() && is_error {
                    failure = Some(output);
                    break;
                }
            }
        }
        drop(stream);
        assert!(failure
            .expect("failure event")
            .contains("without submitting a terminal tool"));
        assert_eq!(ctx.exit_reason, Some(QueryExitReason::TerminalNotSubmitted));
    }

    #[tokio::test]
    async fn terminal_not_submitted_drains_queued_notifications_before_exit() {
        let source = Arc::new(ScriptedSource {
            turns: tokio::sync::Mutex::new(Vec::new()),
        });
        let mut ctx = ctx(source);
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
        let mut saw_notification = false;
        let mut saw_failure = false;
        while let Some(item) = stream.next().await {
            let (event, _) = item.expect("stream item");
            match event {
                StreamEvent::SystemNotification { text, .. } => {
                    saw_notification |= text.contains("[BACKGROUND COMPLETED] cmd_1");
                }
                StreamEvent::ToolExecutionCompleted {
                    tool_name,
                    is_error,
                    ..
                } if tool_name.is_empty() && is_error => {
                    saw_failure = true;
                    break;
                }
                _ => {}
            }
        }
        drop(stream);

        assert!(saw_notification, "queued completion must be streamed");
        assert!(saw_failure, "hard ceiling still emits the failure event");
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
}

//! Query request construction.

use eos_llm_client::{LlmRequest, Message};

use crate::query::{provider_messages::build_provider_messages, QueryContext};

/// Built provider request plus the prompt-report sequence for the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRunRequest {
    /// Provider-neutral request.
    pub request: LlmRequest,
    /// Prompt-report sequence id.
    pub prompt_report_seq: u64,
}

/// Build the provider request for one loop turn.
pub async fn build_query_run_request(
    ctx: &mut QueryContext,
    messages: &[Message],
) -> QueryRunRequest {
    let seq = match &ctx.prompt_report {
        Some(recorder) => recorder.next_seq().await,
        None => 0,
    };
    let provider_messages = build_provider_messages(messages);
    let mut builder = LlmRequest::builder(ctx.model.clone())
        .messages(provider_messages)
        .system_prompt(ctx.system_prompt.clone())
        .max_tokens(ctx.max_tokens)
        .tools(ctx.tool_registry.specs());
    if let Some(effort) = ctx.reasoning_effort {
        builder = builder.reasoning_effort(effort);
    }
    let request = builder.build();
    QueryRunRequest {
        request,
        prompt_report_seq: seq,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use eos_llm_client::{ContentBlock, MessageRole};
    use eos_tools::ToolRegistry;
    use eos_types::{AgentRunId, JsonObject};

    use super::*;
    use crate::notifications::NotificationService;
    use eos_testkit::metadata;

    fn context() -> QueryContext {
        QueryContext {
            tool_registry: Arc::new(ToolRegistry::new()),
            cwd: PathBuf::new(),
            model: "model".to_owned(),
            system_prompt: "system".to_owned(),
            max_tokens: 32,
            reasoning_effort: None,
            tool_call_limit: 8,
            agent_name: "root".to_owned(),
            agent_run_id: AgentRunId::new_v4(),
            task_id: None,
            tool_calls_used: 0,
            text_only_no_terminal_turns: 0,
            tool_metadata: metadata(),
            terminal_tools: BTreeSet::new(),
            exit_reason: None,
            terminal_result: None,
            event_source: None,
            prompt_report: None,
            notification_rules: Vec::new(),
            notification_fired: BTreeSet::new(),
            notifier: NotificationService::new(),
            audit: None,
            run_handles: None,
        }
    }

    #[tokio::test]
    async fn build_request_uses_provider_safe_history() {
        let mut ctx = context();
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                tool_use_id: "toolu_a".parse().expect("tool use id"),
                name: "read_file".to_owned(),
                input: JsonObject::new(),
            }],
        }];

        let request = build_query_run_request(&mut ctx, &messages).await;

        assert!(request.request.messages.is_empty());
        assert_eq!(
            messages[0].content.len(),
            1,
            "durable transcript input is not mutated"
        );
    }
}

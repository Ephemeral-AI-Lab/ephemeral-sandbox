//! Query request construction.

use eos_llm_client::{LlmRequest, Message};

use crate::query::QueryContext;

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
    let request = LlmRequest::builder(ctx.model.clone())
        .messages(messages.to_vec())
        .system_prompt(ctx.system_prompt.clone())
        .max_tokens(ctx.max_tokens)
        .tools(ctx.tool_registry.specs())
        .build();
    QueryRunRequest {
        request,
        prompt_report_seq: seq,
    }
}

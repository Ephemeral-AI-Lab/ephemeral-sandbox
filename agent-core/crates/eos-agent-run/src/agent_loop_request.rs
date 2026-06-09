//! Runner-owned conversion from agent definitions to loop requests.

use eos_engine::{AgentLoopMessage, StartAgentLoopRequest};
use eos_llm_client::{Message, MessageRole, DEFAULT_MAX_TOKENS};
use eos_types::{AgentDefinition, AgentRunId, SpawnAgentRequest};

/// Build the thin engine loop request for one resolved agent.
#[must_use]
pub fn build_start_agent_loop_request(
    agent: &AgentDefinition,
    request: SpawnAgentRequest,
    agent_run_id: AgentRunId,
) -> StartAgentLoopRequest {
    StartAgentLoopRequest {
        agent_run_id,
        initial_messages: initial_loop_messages(agent, request.initial_messages),
        model_key: agent.model.clone().unwrap_or_default(),
        max_completion_tokens: DEFAULT_MAX_TOKENS,
        tool_call_limit: agent.tool_call_limit.get(),
    }
}

fn initial_loop_messages(
    agent: &AgentDefinition,
    initial_messages: Vec<Message>,
) -> Vec<AgentLoopMessage> {
    let mut messages = Vec::with_capacity(initial_messages.len().saturating_add(1));
    if let Some(system_prompt) = agent
        .system_prompt
        .as_ref()
        .filter(|text| !text.trim().is_empty())
    {
        messages.push(AgentLoopMessage::SystemPrompt(system_prompt.clone()));
    }
    messages.extend(
        initial_messages
            .into_iter()
            .map(|message| match message.role {
                MessageRole::User => AgentLoopMessage::UserMessage(message),
                MessageRole::Assistant => AgentLoopMessage::AssistantMessage(message),
            }),
    );
    messages
}

//! Scripted LLM doubles: a [`ProviderStreamSource`] that replays queued turns, the
//! [`ProviderStreamSourceFactory`] builders that dispatch scripts per agent, and the
//! `tool_use_turn` / `text_turn` helpers that fabricate one model turn.
//!
//! This is the single definition of `ScriptedSource` in the workspace
//! (`TESTING_SPEC` AC3); the prior crate-local copies are gone.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_engine::{
    AgentRunStreamEvent, AssistantMessageComplete, EngineError, EngineStream, ProviderStreamSource,
    ProviderStreamSourceFactory,
};
use eos_llm_client::{ContentBlock, LlmRequest, Message, MessageRole, UsageSnapshot};

/// A scripted provider stream source: each `stream()` call replays the next queued turn.
/// When `block_when_empty` is set, an exhausted source blocks forever instead of
/// returning an empty turn (keeps the agent "running" for park-and-inspect
/// tests).
#[derive(Debug)]
pub struct ScriptedSource {
    turns: tokio::sync::Mutex<Vec<Vec<AgentRunStreamEvent>>>,
    block_when_empty: bool,
}

impl ScriptedSource {
    /// Replay `turns` in order; an exhausted source returns an empty turn.
    #[must_use]
    pub fn new(turns: Vec<Vec<AgentRunStreamEvent>>) -> Self {
        Self {
            turns: tokio::sync::Mutex::new(turns),
            block_when_empty: false,
        }
    }

    /// Replay `turns`, then block forever (the agent stays running).
    #[must_use]
    pub fn new_blocking(turns: Vec<Vec<AgentRunStreamEvent>>) -> Self {
        Self {
            turns: tokio::sync::Mutex::new(turns),
            block_when_empty: true,
        }
    }
}

#[async_trait]
impl ProviderStreamSource for ScriptedSource {
    async fn stream(&self, _request: &LlmRequest) -> Result<EngineStream, EngineError> {
        let mut turns = self.turns.lock().await;
        if turns.is_empty() {
            if self.block_when_empty {
                drop(turns);
                std::future::pending::<()>().await;
                unreachable!("pending future never resolves");
            }
            return Ok(Box::pin(futures::stream::iter(Vec::new())));
        }
        let events = turns.remove(0);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

/// A factory that always returns the given scripted turns.
#[must_use]
pub fn factory_from(turns: Vec<Vec<AgentRunStreamEvent>>) -> ProviderStreamSourceFactory {
    Arc::new(move |_request, _agent_state| {
        Arc::new(ScriptedSource::new(turns.clone())) as Arc<dyn ProviderStreamSource>
    })
}

/// A factory where the `root` agent plays `root_turns` then blocks (stays
/// running), and every other agent gets an empty (first-turn-erroring) source.
#[must_use]
pub fn factory_root_blocks_after(
    root_turns: Vec<Vec<AgentRunStreamEvent>>,
) -> ProviderStreamSourceFactory {
    Arc::new(move |_request, agent_state| {
        if agent_state.agent_name == "root" {
            Arc::new(ScriptedSource::new_blocking(root_turns.clone()))
                as Arc<dyn ProviderStreamSource>
        } else {
            Arc::new(ScriptedSource::new(Vec::new())) as Arc<dyn ProviderStreamSource>
        }
    })
}

/// A factory that dispatches scripted turns by agent name; an agent absent from
/// the map gets an empty (first-turn-erroring) source.
#[must_use]
pub fn factory_by_agent(
    by_agent: Vec<(&'static str, Vec<Vec<AgentRunStreamEvent>>)>,
) -> ProviderStreamSourceFactory {
    let scripts: HashMap<String, Vec<Vec<AgentRunStreamEvent>>> = by_agent
        .into_iter()
        .map(|(name, turns)| (name.to_owned(), turns))
        .collect();
    Arc::new(move |_request, agent_state| {
        let turns = scripts
            .get(&agent_state.agent_name)
            .cloned()
            .unwrap_or_default();
        Arc::new(ScriptedSource::new(turns)) as Arc<dyn ProviderStreamSource>
    })
}

/// One model turn that calls `tool_name` with `input` (a non-object `input`
/// lowers to an empty object).
#[must_use]
pub fn tool_use_turn(
    tool_use_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> Vec<AgentRunStreamEvent> {
    let input = match input {
        serde_json::Value::Object(map) => map,
        _ => eos_types::JsonObject::new(),
    };
    vec![AgentRunStreamEvent::AssistantMessageComplete {
        agent_name: String::new(),
        agent_run_id: None,
        payload: Box::new(AssistantMessageComplete {
            message: Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolUse {
                    tool_use_id: tool_use_id.parse().expect("tool use id"),
                    name: tool_name.to_owned(),
                    input,
                }],
            },
            usage: UsageSnapshot::default(),
            stop_reason: None,
        }),
    }]
}

/// One assistant text turn (no tool call).
#[must_use]
pub fn text_turn(text: &str) -> Vec<AgentRunStreamEvent> {
    vec![AgentRunStreamEvent::AssistantMessageComplete {
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
    }]
}

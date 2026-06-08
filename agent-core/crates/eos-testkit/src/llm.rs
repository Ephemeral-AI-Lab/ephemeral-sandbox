//! Scripted LLM doubles: an [`EventSource`] that replays queued turns, the
//! [`EventSourceFactory`] builders that dispatch scripts per agent, and the
//! `tool_use_turn` / `text_turn` helpers that fabricate one model turn.
//!
//! This is the single definition of `ScriptedSource` in the workspace
//! (`TESTING_SPEC` AC3); the prior `eos-engine` and `eos-runtime` copies are gone.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_engine::{
    AssistantMessageComplete, EngineError, EngineStream, EventSource, EventSourceFactory,
    StreamEvent,
};
use eos_llm_client::{ContentBlock, LlmRequest, Message, MessageRole, UsageSnapshot};

/// A scripted event source: each `stream()` call replays the next queued turn.
/// When `block_when_empty` is set, an exhausted source blocks forever instead of
/// returning an empty turn (keeps the agent "running" for park-and-inspect
/// tests).
#[derive(Debug)]
pub struct ScriptedSource {
    turns: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
    block_when_empty: bool,
}

impl ScriptedSource {
    /// Replay `turns` in order; an exhausted source returns an empty turn.
    #[must_use]
    pub fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            turns: tokio::sync::Mutex::new(turns),
            block_when_empty: false,
        }
    }

    /// Replay `turns`, then block forever (the agent stays running).
    #[must_use]
    pub fn new_blocking(turns: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            turns: tokio::sync::Mutex::new(turns),
            block_when_empty: true,
        }
    }
}

#[async_trait]
impl EventSource for ScriptedSource {
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

/// A scripted event source that routes each model request by its terminal tool
/// set. Tests keep using agent-name keys such as `root`, `advisor`, `planner`,
/// `coder`, `reducer`, and `explorer` without exposing `AgentDefinition` to the
/// engine event-source seam.
#[derive(Debug)]
pub struct ScriptedByAgentSource {
    scripts: tokio::sync::Mutex<HashMap<String, Vec<Vec<StreamEvent>>>>,
    block_when_empty: HashMap<String, bool>,
}

impl ScriptedByAgentSource {
    /// Build a routed source from per-agent scripts.
    #[must_use]
    pub fn new(scripts: HashMap<String, Vec<Vec<StreamEvent>>>) -> Self {
        Self {
            scripts: tokio::sync::Mutex::new(scripts),
            block_when_empty: HashMap::new(),
        }
    }

    /// Mark one route as blocking after its scripted turns are exhausted.
    #[must_use]
    pub fn with_blocking_route(mut self, agent_name: impl Into<String>) -> Self {
        self.block_when_empty.insert(agent_name.into(), true);
        self
    }
}

#[async_trait]
impl EventSource for ScriptedByAgentSource {
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
        let mut scripts = self.scripts.lock().await;
        let key = request_route_key(&scripts, request);
        let turns = scripts.entry(key.clone()).or_default();
        if turns.is_empty() {
            if self.block_when_empty.get(&key).copied().unwrap_or(false) {
                drop(scripts);
                std::future::pending::<()>().await;
                unreachable!("pending future never resolves");
            }
            return Ok(Box::pin(futures::stream::iter(Vec::new())));
        }
        let events = turns.remove(0);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

/// Return the scripted agent route implied by the request's terminal tool set,
/// constrained to routes the caller can serve.
#[must_use]
pub fn request_route_key_for(request: &LlmRequest, available_routes: &[&str]) -> String {
    let candidates = request_route_candidates(request);
    for &candidate in candidates {
        if available_routes.contains(&candidate) {
            return candidate.to_owned();
        }
    }
    available_routes
        .first()
        .copied()
        .unwrap_or("root")
        .to_owned()
}

/// A factory that always returns the given scripted turns.
#[must_use]
pub fn factory_from(turns: Vec<Vec<StreamEvent>>) -> EventSourceFactory {
    Arc::new(move |_request| Arc::new(ScriptedSource::new(turns.clone())) as Arc<dyn EventSource>)
}

/// A factory where the `root` agent plays `root_turns` then blocks (stays
/// running), and every other agent gets an empty (first-turn-erroring) source.
#[must_use]
pub fn factory_root_blocks_after(root_turns: Vec<Vec<StreamEvent>>) -> EventSourceFactory {
    Arc::new(move |_request| {
        let scripts = HashMap::from([("root".to_owned(), root_turns.clone())]);
        Arc::new(ScriptedByAgentSource::new(scripts).with_blocking_route("root"))
            as Arc<dyn EventSource>
    })
}

/// A factory that dispatches scripted turns by agent name; an agent absent from
/// the map gets an empty (first-turn-erroring) source.
#[must_use]
pub fn factory_by_agent(
    by_agent: Vec<(&'static str, Vec<Vec<StreamEvent>>)>,
) -> EventSourceFactory {
    let scripts: HashMap<String, Vec<Vec<StreamEvent>>> = by_agent
        .into_iter()
        .map(|(name, turns)| (name.to_owned(), turns))
        .collect();
    Arc::new(move |_request| {
        Arc::new(ScriptedByAgentSource::new(scripts.clone())) as Arc<dyn EventSource>
    })
}

fn request_route_key(
    scripts: &HashMap<String, Vec<Vec<StreamEvent>>>,
    request: &LlmRequest,
) -> String {
    let candidates = request_route_candidates(request);
    for &candidate in candidates {
        if scripts.contains_key(candidate) {
            return candidate.to_owned();
        }
    }
    candidates.first().copied().unwrap_or("root").to_owned()
}

fn request_route_candidates(request: &LlmRequest) -> &'static [&'static str] {
    let has_tool = |name: &str| request.tools.iter().any(|tool| tool.name == name);
    if has_tool("submit_advisor_feedback") {
        return &["advisor"];
    }
    if has_tool("submit_planner_outcome") {
        return &["planner"];
    }
    if has_tool("submit_generator_outcome") {
        return &["coder", "generator"];
    }
    if has_tool("submit_reducer_outcome") {
        return &["reducer"];
    }
    if has_tool("submit_exploration_result") {
        return &["explorer", "subagent"];
    }
    if has_tool("submit_root_outcome") {
        return &["root"];
    }
    &["root"]
}

/// One model turn that calls `tool_name` with `input` (a non-object `input`
/// lowers to an empty object).
#[must_use]
pub fn tool_use_turn(
    tool_use_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> Vec<StreamEvent> {
    let input = match input {
        serde_json::Value::Object(map) => map,
        _ => eos_types::JsonObject::new(),
    };
    vec![StreamEvent::AssistantMessageComplete {
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
pub fn text_turn(text: &str) -> Vec<StreamEvent> {
    vec![StreamEvent::AssistantMessageComplete {
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

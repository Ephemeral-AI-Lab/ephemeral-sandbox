//! Agent-loop launch contracts shared by runner and engine.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::contracts::AgentRunApi;
use crate::ids::AgentRunId;
use crate::json::JsonObject;
use crate::llm::Message;

/// Boxed terminal outcome future returned after an agent loop is launched.
pub type AgentLoopOutcomeFuture =
    Pin<Box<dyn Future<Output = Option<AgentLoopOutcome>> + Send + 'static>>;

/// Shared cancellation handle for a running agent loop.
pub type AgentLoopCancellationHandle = Arc<dyn AgentLoopCancellation>;

/// Thin request to start one agent loop.
#[derive(Debug, Clone)]
pub struct StartAgentLoopRequest {
    /// Agent-run id.
    pub agent_run_id: AgentRunId,
    /// Runner-prepared initial messages.
    pub initial_messages: Vec<AgentLoopMessage>,
    /// Resolved model key.
    pub model_key: String,
    /// Completion token cap.
    pub max_completion_tokens: u32,
    /// Tool-call limit.
    pub tool_call_limit: u32,
}

/// Engine-loop message wrapper.
#[derive(Debug, Clone)]
pub enum AgentLoopMessage {
    /// System prompt text.
    SystemPrompt(String),
    /// User message.
    UserMessage(Message),
    /// Assistant message.
    AssistantMessage(Message),
}

/// Terminal loop outcome envelope.
#[derive(Debug, Clone)]
pub struct AgentLoopOutcome {
    /// Outcome kind.
    pub kind: AgentLoopOutcomeKind,
    /// Final loop transcript.
    pub final_conversation_messages: Vec<AgentLoopMessage>,
    /// Total provider token count when known.
    pub total_token_count: Option<i64>,
}

/// Narrow loop outcome kinds.
#[derive(Debug, Clone)]
pub enum AgentLoopOutcomeKind {
    /// A terminal tool submitted successfully.
    TerminalToolSubmitted {
        /// Persistable terminal submission payload.
        submission_payload: JsonObject,
    },
    /// The loop failed or exited without a valid terminal submission.
    LoopFailed {
        /// Human-readable error summary.
        error_summary: String,
    },
}

/// Cancellation behavior for one running agent loop.
pub trait AgentLoopCancellation: Send + Sync {
    /// Request loop cancellation. Implementations should keep the first reason.
    fn cancel(&self, reason: &str);
}

/// Handle returned after an agent loop has been started.
pub struct StartedAgentLoop {
    /// Resolves when the loop publishes a terminal outcome.
    pub outcome: AgentLoopOutcomeFuture,
    /// Cooperative cancellation handle for the running loop.
    pub cancellation: AgentLoopCancellationHandle,
}

impl std::fmt::Debug for StartedAgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedAgentLoop").finish_non_exhaustive()
    }
}

/// Public non-blocking launcher for agent loops.
pub trait AgentLoopLauncher: Send + Sync {
    /// Start an agent loop and return immediately with its outcome handle.
    fn start_agent_loop(
        &self,
        request: StartAgentLoopRequest,
        agent_run_api: Arc<dyn AgentRunApi>,
    ) -> StartedAgentLoop;
}

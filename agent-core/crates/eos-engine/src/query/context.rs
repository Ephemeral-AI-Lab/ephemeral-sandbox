//! Query-loop context and event-source seam.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::LlmRequest;
use eos_tools::{ExecutionMetadata, ToolName, ToolRegistry, ToolResult};
use eos_types::{AgentRunId, JsonObject, TaskId};
use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::agent_loop::EngineRunHandles;
use crate::{
    EngineError, NotificationRule, NotificationService, PromptReportRecorder, StreamEvent,
};

/// The engine stream returned by one model turn.
pub type EngineStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send>>;

/// A per-agent stream source. Production adapts an `LlmClient`; tests can replay
/// scripted engine events while still exercising the real loop.
#[async_trait]
pub trait EventSource: Send + Sync {
    /// Open one model turn for `request`.
    ///
    /// # Errors
    /// Returns [`EngineError`] for request construction or stream setup faults.
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError>;
}

/// Why the query loop exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryExitReason {
    /// A successful terminal tool was submitted.
    ToolStop,
    /// The hard no-terminal ceiling was reached.
    TerminalNotSubmitted,
}

/// Mutable state for one agent query loop.
#[derive(Clone)]
pub struct QueryContext {
    /// Immutable tool registry for this agent.
    pub tool_registry: Arc<ToolRegistry>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Resolved model key.
    pub model: String,
    /// Request system prompt.
    pub system_prompt: String,
    /// Completion token cap.
    pub max_tokens: u32,
    /// Configured tool-call limit.
    pub tool_call_limit: u32,
    /// Agent profile name.
    pub agent_name: String,
    /// Agent run id.
    pub agent_run_id: AgentRunId,
    /// Owning task id, when known.
    pub task_id: Option<TaskId>,
    /// Counted tool calls.
    pub tool_calls_used: u32,
    /// Counted text-only turns without terminal submission.
    pub text_only_no_terminal_turns: u32,
    /// Tool execution metadata cloned per call.
    pub tool_metadata: ExecutionMetadata,
    /// Whether engine-dispatched background tasks are enabled.
    pub enable_background_tasks: bool,
    /// Terminal tools visible to this agent.
    pub terminal_tools: BTreeSet<ToolName>,
    /// Loop exit reason.
    pub exit_reason: Option<QueryExitReason>,
    /// Terminal result, when one was produced.
    pub terminal_result: Option<ToolResult>,
    /// Event-source seam.
    pub event_source: Option<Arc<dyn EventSource>>,
    /// Optional prompt-report recorder.
    pub prompt_report: Option<PromptReportRecorder>,
    /// Declarative notification rules.
    pub notification_rules: Vec<Arc<dyn NotificationRule>>,
    /// Fire-once notification names already emitted.
    pub notification_fired: BTreeSet<String>,
    /// Per-rule scratchpad.
    pub notification_state: JsonObject,
    /// The per-request notification sink the loop drains at the top of every
    /// turn (anchor §6). Shares its queue with the tool/heartbeat sink — the
    /// instance-identity invariant (anchor §7): if these diverge it compiles and
    /// silently delivers nothing.
    pub notifier: NotificationService,
    /// The explicit run handles the engine-driven advisor dispatch needs to spawn
    /// a child `run_ephemeral_agent` (advisor remediation plan §2a). `None` in
    /// tests that never exercise `ask_advisor`; the gate itself reads only the
    /// transcript, never these handles.
    pub run_handles: Option<EngineRunHandles>,
}

impl std::fmt::Debug for QueryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryContext")
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("tool_call_limit", &self.tool_call_limit)
            .field("agent_name", &self.agent_name)
            .field("agent_run_id", &self.agent_run_id)
            .field("task_id", &self.task_id)
            .field("tool_calls_used", &self.tool_calls_used)
            .field(
                "text_only_no_terminal_turns",
                &self.text_only_no_terminal_turns,
            )
            .field("enable_background_tasks", &self.enable_background_tasks)
            .field("terminal_tools", &self.terminal_tools)
            .field("exit_reason", &self.exit_reason)
            .finish_non_exhaustive()
    }
}

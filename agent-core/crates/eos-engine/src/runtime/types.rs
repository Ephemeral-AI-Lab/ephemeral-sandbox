//! Engine run contracts supplied by the runtime composition root.

use std::sync::Arc;

use eos_agent_def::{AgentDefinition, AgentRegistry};
use eos_agent_message_records::{AgentMessageRecords, AgentRunRecordKind};
use eos_agent_run::AgentRunApi;
use eos_audit::AuditSink;
use eos_llm_client::{LlmClient, Message};
use eos_state::AgentRunStore;
use eos_tools::{
    AttemptSubmissionService, CommandSessionPort, ExecutionMetadata, RootSubmissionService,
    SandboxToolService, SkillToolService, SubagentSessionPort, ToolConfigSet, ToolRegistry,
    ToolResult, WorkflowServicePort, WorkflowSessionPort,
};
use eos_types::{AgentRunId, TaskId};

use crate::background::BackgroundTeardownPort;
use crate::notifications::NotificationService;
use crate::query::EventSource;
use crate::telemetry::StreamEvent;

use super::control::AgentRunCancellation;
use super::foreground::ForegroundExecutor;
use super::registry::AgentRunRegistry;

/// Per-agent event-source factory seam.
///
/// `None` selects the live provider stream; the mock harness sets it so each
/// spawned agent runs the real loop against a scripted source. Owned here (next
/// to [`EventSource`]) so the engine-driven advisor run can resolve a source
/// without a runtime back-edge.
pub type EventSourceFactory = Arc<dyn Fn(&AgentDefinition) -> Arc<dyn EventSource> + Send + Sync>;

/// Per-run stream-event callback.
pub type EventCallback = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// Runtime-supplied extension point for non-core model tools, such as plugin
/// catalog tools. The engine owns per-agent registry construction but stays
/// ignorant of plugin catalog internals.
pub type ToolRegistryExtender = Arc<dyn Fn(&mut ToolRegistry) + Send + Sync>;

/// The explicit run handles `run_agent` needs, in place of a runtime-wide state
/// bag. Cheap to clone (every field is an `Arc` or a small value); it rides on
/// the [`QueryContext`](crate::query::QueryContext) so the advisor dispatch path
/// can spawn a child run with the same handles.
#[derive(Clone)]
pub struct EngineRunHandles {
    /// Agent-run persistence (create/finish rows).
    pub agent_run_store: Arc<dyn AgentRunStore>,
    /// Provider client for the production event source.
    pub llm_client: Arc<dyn LlmClient>,
    /// Per-agent event-source override (mock harness); `None` uses `llm_client`.
    pub event_source_factory: Option<EventSourceFactory>,
    /// Agent registry (caller scope + advisor `AgentDefinition` resolution).
    pub agent_registry: Arc<AgentRegistry>,
    /// Externalized tool config (`.eos-agents/tools`), loaded once at composition
    /// and read by `build_default_registry` for every per-agent registry.
    pub tool_config: Arc<ToolConfigSet>,
    /// Sandbox RPC service captured by file/shell/plugin/isolated tools.
    pub sandbox_service: SandboxToolService,
    /// Root terminal store service. Present for request roots; tests/static
    /// harnesses may omit it.
    pub root_submission: Option<RootSubmissionService>,
    /// Skill registry service captured by `load_skill_reference`.
    pub skill_service: SkillToolService,
    /// Optional runtime extender that registers dynamic plugin tools into each
    /// per-agent registry after the built-in tools are registered.
    pub tool_registry_extender: Option<ToolRegistryExtender>,
    /// Agent-core observability sink.
    pub audit: Arc<dyn AuditSink>,
    /// Optional file-backed agent-node message-record service.
    pub message_records: Option<AgentMessageRecords>,
    /// Request-visible workspace root used as the engine/provider cwd.
    pub workspace_root: String,
}

impl std::fmt::Debug for EngineRunHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineRunHandles")
            .field("workspace_root", &self.workspace_root)
            .field(
                "has_event_source_factory",
                &self.event_source_factory.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// Inputs for [`run_agent`](crate::run_agent).
pub struct AgentRunInput {
    /// The resolved agent definition to run.
    pub agent: AgentDefinition,
    /// The seed transcript (typically one user message).
    pub initial_messages: Vec<Message>,
    /// The owning task, when persisting the agent run.
    pub task_id: Option<TaskId>,
    /// The agent-run id minted for this run.
    pub agent_run_id: AgentRunId,
    /// The typed tool execution context threaded through every tool call.
    pub tool_metadata: ExecutionMetadata,
    /// Per-attempt terminal submission service for planner/generator/reducer
    /// agents. `None` for root/helper runs.
    pub attempt_submission: Option<AttemptSubmissionService>,
    /// Agent-run service for subagent launch tools.
    pub agent_run_service: Option<Arc<dyn AgentRunApi>>,
    /// Subagent background-session registry for this run.
    pub subagent_sessions: Option<Arc<dyn SubagentSessionPort>>,
    /// Workflow service for workflow tools and workflow-state hooks.
    pub workflow_service: Option<Arc<dyn WorkflowServicePort>>,
    /// Workflow background-session registry for this run.
    pub workflow_sessions: Option<Arc<dyn WorkflowSessionPort>>,
    /// Background teardown for run-finalization cleanup.
    pub background_session: Option<Arc<dyn BackgroundTeardownPort>>,
    /// Command-session lifecycle port for shell tools.
    pub command_session_port: Option<Arc<dyn CommandSessionPort>>,
    /// The run-local notification sink owned by this run's `AgentRunControl` and
    /// shared (by clone) with tools, the heartbeat, and the query loop — the §7
    /// instance-identity invariant. Helper runs pass a fresh standalone service.
    pub notifier: NotificationService,
    /// The run's cooperative cancellation token (a clone of the one owned by
    /// `AgentRunControl`). The query loop polls it at turn boundaries.
    pub cancellation: AgentRunCancellation,
    /// The run's foreground cancelable-effect registry (shared from
    /// `AgentRunControl`), threaded onto the `QueryContext` for cancellation.
    pub foreground: Arc<ForegroundExecutor>,
    /// The live-run registry, **only for registered (root/workflow) runs**. When
    /// `Some`, `run_agent` claims the registry entry (`Running -> Claimed`) before
    /// finalizing the `agent_run` row + child teardown, so a concurrent
    /// `cancel_agent_run` cannot double-finalize. Helper/subagent runs that were
    /// never inserted pass `None` and finalize naturally (spec §6.4, finalization
    /// arbitration).
    pub agent_run_registry: Option<AgentRunRegistry>,
    /// Whether to record an `agent_run` row (create + finish).
    pub persist_agent_run: bool,
    /// Message-record node kind and parent/location facts for this run.
    pub record_kind: AgentRunRecordKind,
}

impl std::fmt::Debug for AgentRunInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunInput")
            .field("agent", &self.agent.name)
            .field("initial_messages", &self.initial_messages.len())
            .field("task_id", &self.task_id)
            .field("agent_run_id", &self.agent_run_id)
            .field("has_attempt_submission", &self.attempt_submission.is_some())
            .field("has_agent_run_service", &self.agent_run_service.is_some())
            .field("has_subagent_sessions", &self.subagent_sessions.is_some())
            .field("has_workflow_service", &self.workflow_service.is_some())
            .field("has_workflow_sessions", &self.workflow_sessions.is_some())
            .field("has_background_session", &self.background_session.is_some())
            .field(
                "has_command_session_port",
                &self.command_session_port.is_some(),
            )
            .field("persist_agent_run", &self.persist_agent_run)
            .field("record_kind", &self.record_kind)
            .finish_non_exhaustive()
    }
}

/// The result of one agent run, read from the loop's `QueryContext`.
#[derive(Debug)]
pub struct AgentRunResult {
    /// The terminal tool result, when a terminal tool succeeded.
    pub terminal_result: Option<ToolResult>,
    /// A framework-fault summary if context construction or the stream broke.
    pub error: Option<String>,
}

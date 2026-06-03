//! The single "drive one ephemeral agent" loop driver — the engine primitive
//! `run_ephemeral_agent`, relocated here from `eos-runtime` (advisor remediation
//! plan §2a).
//!
//! It wraps the engine's own `build_query_context` + `run_query` seam, so it is
//! an engine primitive, not a runtime concern. It no longer takes `&AppState`
//! (constraint 6): the explicit run handles it needs ride in [`EngineRunHandles`],
//! which `eos-runtime` builds from its composition root and the advisor dispatch
//! path reads off [`QueryContext::run_handles`](crate::query::QueryContext).

use std::path::PathBuf;
use std::sync::Arc;

use eos_agent_def::{AgentDefinition, AgentRegistry};
use eos_llm_client::{LlmClient, Message, DEFAULT_MAX_TOKENS};
use eos_state::{AgentRunStore, ModelStore};
use eos_tools::{build_default_registry, CallerScope, ExecutionMetadata, ToolResult};
use eos_types::{AgentRunId, TaskId};
use futures::StreamExt;

use crate::agent::{build_query_context, BuildQueryContextInput};
use crate::events::StreamEvent;
use crate::notifications::NotificationService;
use crate::query::{run_query, EventSource};

/// Per-agent event-source factory seam (the Python `event_source_factory`).
///
/// `None` selects the live provider stream; the mock harness sets it so each
/// spawned agent runs the real loop against a scripted source. Owned here (next
/// to [`EventSource`]) so the engine-driven advisor run can resolve a source
/// without a runtime back-edge.
pub type EventSourceFactory = Arc<dyn Fn(&AgentDefinition) -> Arc<dyn EventSource> + Send + Sync>;

/// Per-run stream-event callback (the Python `AgentStreamEmitter`).
pub type EventCallback = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// The explicit run handles `run_ephemeral_agent` needs, in place of `&AppState`
/// (constraint 6). Cheap to clone (every field is an `Arc` or a small value); it
/// rides on the [`QueryContext`](crate::query::QueryContext) so the advisor
/// dispatch path can spawn a child run with the same handles.
#[derive(Clone)]
pub struct EngineRunHandles {
    /// Agent-run persistence (create/finish rows).
    pub agent_run_store: Arc<dyn AgentRunStore>,
    /// Model registry (resolve the active model when the agent has none).
    pub model_store: Arc<dyn ModelStore>,
    /// Provider client for the production event source.
    pub llm_client: Arc<dyn LlmClient>,
    /// Per-agent event-source override (mock harness); `None` uses `llm_client`.
    pub event_source_factory: Option<EventSourceFactory>,
    /// Agent registry (caller scope + advisor `AgentDefinition` resolution).
    pub agent_registry: Arc<AgentRegistry>,
    /// Working directory.
    pub cwd: String,
}

impl std::fmt::Debug for EngineRunHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineRunHandles")
            .field("cwd", &self.cwd)
            .field("has_event_source_factory", &self.event_source_factory.is_some())
            .finish_non_exhaustive()
    }
}

/// Inputs for [`run_ephemeral_agent`].
#[derive(Debug)]
pub struct EphemeralRunInput {
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
    /// The per-request notification sink shared with the tools/heartbeat; the
    /// loop drains it each turn (anchor §7 instance identity). Ephemeral helper
    /// runs (e.g. the advisor) pass a fresh, standalone service.
    pub notifier: NotificationService,
    /// Whether to record an `agent_run` row (create + finish).
    pub persist_agent_run: bool,
}

/// The result of one ephemeral agent run, read from the loop's `QueryContext`.
#[derive(Debug)]
pub struct EphemeralRun {
    /// The terminal tool result, when a terminal tool succeeded.
    pub terminal_result: Option<ToolResult>,
    /// A framework-fault summary if context construction or the stream broke.
    pub error: Option<String>,
}

/// Drive one ephemeral agent to completion.
pub async fn run_ephemeral_agent(
    handles: &EngineRunHandles,
    input: EphemeralRunInput,
    on_event: Option<&EventCallback>,
) -> EphemeralRun {
    let EphemeralRunInput {
        agent,
        mut initial_messages,
        task_id,
        agent_run_id,
        tool_metadata,
        notifier,
        persist_agent_run,
    } = input;

    let persisted = persist_agent_run && task_id.is_some();
    if let (true, Some(tid)) = (persisted, task_id.as_ref()) {
        if let Err(err) = handles
            .agent_run_store
            .create_run(&agent_run_id, tid, agent.name.as_str(), None)
            .await
        {
            tracing::warn!(error = %err, "agent_run create_run failed (non-fatal)");
        }
    }

    let model = match agent.model.clone() {
        Some(model) => model,
        None => handles
            .model_store
            .active()
            .await
            .ok()
            .flatten()
            .map(|registration| registration.model_key)
            .unwrap_or_else(|| "default-model".to_owned()),
    };
    let event_source: Option<Arc<dyn EventSource>> = handles
        .event_source_factory
        .as_ref()
        .map(|factory| factory(&agent));
    let caller_scope = CallerScope {
        dispatchable_subagents: handles
            .agent_registry
            .dispatchable_subagent_names()
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect(),
    };
    let registry = build_default_registry(&caller_scope);

    let ctx_result = build_query_context(BuildQueryContextInput {
        agent,
        model,
        client: Some(handles.llm_client.clone()),
        event_source,
        registry,
        base_system_prompt: String::new(),
        max_tokens: DEFAULT_MAX_TOKENS,
        cwd: PathBuf::from(&handles.cwd),
        agent_run_id: agent_run_id.clone(),
        task_id,
        tool_metadata,
        notifier,
        run_handles: Some(handles.clone()),
    });

    let mut ctx = match ctx_result {
        Ok(ctx) => ctx,
        Err(err) => {
            let summary = err.to_string();
            if persisted {
                finish_run(handles, &agent_run_id, Some(&summary)).await;
            }
            return EphemeralRun {
                terminal_result: None,
                error: Some(summary),
            };
        }
    };

    let mut error: Option<String> = None;
    {
        let mut stream = run_query(&mut ctx, &mut initial_messages);
        while let Some(item) = stream.next().await {
            match item {
                Ok((event, _usage)) => {
                    if let Some(callback) = on_event {
                        callback(&event);
                    }
                }
                Err(err) => {
                    error = Some(err.to_string());
                    break;
                }
            }
        }
    }

    let terminal_result = ctx.terminal_result.clone();
    if persisted {
        finish_run(handles, &agent_run_id, error.as_deref()).await;
    }
    EphemeralRun {
        terminal_result,
        error,
    }
}

async fn finish_run(handles: &EngineRunHandles, agent_run_id: &AgentRunId, error: Option<&str>) {
    if let Err(err) = handles
        .agent_run_store
        .finish_run(agent_run_id, None, None, 0, error)
        .await
    {
        tracing::warn!(error = %err, "agent_run finish_run failed (non-fatal)");
    }
}

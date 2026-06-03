//! The single "drive one ephemeral agent" loop driver, shared by the root-agent
//! path ([`crate::root_agent`]) and the delegated-workflow `AgentRunner` adapter
//! ([`crate::agent_runner`]).
//!
//! `eos-engine` exposes the low-level seam (`build_query_context` + `run_query`
//! stream + post-hoc `ctx.exit_reason`/`ctx.terminal_result`), not a single
//! `run_ephemeral_agent`. This module is that missing wrapper (the documented
//! Phase-6 residual): it builds the per-run context, drives the loop to
//! exhaustion forwarding stream events, records the agent run, and reports the
//! exit reason + terminal result.

use std::path::PathBuf;
use std::sync::Arc;

use eos_agent_def::AgentDefinition;
use eos_engine::{build_query_context, run_query, BuildQueryContextInput, EventSource};
use eos_llm_client::{Message, DEFAULT_MAX_TOKENS};
use eos_tools::{build_default_registry, CallerScope, ExecutionMetadata, ToolResult};
use eos_types::{AgentRunId, TaskId};
use futures::StreamExt;

use crate::app_state::{AppState, EventCallback};

/// Inputs for [`run_ephemeral_agent`].
pub(crate) struct EphemeralRunInput {
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
    /// Whether to record an `agent_run` row (create + finish).
    pub persist_agent_run: bool,
}

/// The result of one ephemeral agent run, read from the loop's `QueryContext`.
pub(crate) struct EphemeralRun {
    /// The terminal tool result, when a terminal tool succeeded.
    pub terminal_result: Option<ToolResult>,
    /// A framework-fault summary if context construction or the stream broke.
    pub error: Option<String>,
}

/// Drive one ephemeral agent to completion.
pub(crate) async fn run_ephemeral_agent(
    state: &AppState,
    input: EphemeralRunInput,
    on_event: Option<&EventCallback>,
) -> EphemeralRun {
    let EphemeralRunInput {
        agent,
        mut initial_messages,
        task_id,
        agent_run_id,
        tool_metadata,
        persist_agent_run,
    } = input;

    let persisted = persist_agent_run && task_id.is_some();
    if let (true, Some(tid)) = (persisted, task_id.as_ref()) {
        if let Err(err) = state
            .agent_run_store
            .create_run(&agent_run_id, tid, agent.name.as_str(), None)
            .await
        {
            tracing::warn!(error = %err, "agent_run create_run failed (non-fatal)");
        }
    }

    let model = match agent.model.clone() {
        Some(model) => model,
        None => state
            .model_store
            .active()
            .await
            .ok()
            .flatten()
            .map(|registration| registration.model_key)
            .unwrap_or_else(|| "default-model".to_owned()),
    };
    let event_source: Option<Arc<dyn EventSource>> = state
        .event_source_factory
        .as_ref()
        .map(|factory| factory(&agent));
    let caller_scope = CallerScope {
        dispatchable_subagents: state
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
        client: Some(state.llm_client.clone()),
        event_source,
        registry,
        base_system_prompt: String::new(),
        max_tokens: DEFAULT_MAX_TOKENS,
        cwd: PathBuf::from(&state.cwd),
        agent_run_id: agent_run_id.clone(),
        task_id,
        tool_metadata,
    });

    let mut ctx = match ctx_result {
        Ok(ctx) => ctx,
        Err(err) => {
            let summary = err.to_string();
            if persisted {
                finish_run(state, &agent_run_id, Some(&summary)).await;
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
        finish_run(state, &agent_run_id, error.as_deref()).await;
    }
    EphemeralRun {
        terminal_result,
        error,
    }
}

async fn finish_run(state: &AppState, agent_run_id: &AgentRunId, error: Option<&str>) {
    if let Err(err) = state
        .agent_run_store
        .finish_run(agent_run_id, None, None, 0, error)
        .await
    {
        tracing::warn!(error = %err, "agent_run finish_run failed (non-fatal)");
    }
}

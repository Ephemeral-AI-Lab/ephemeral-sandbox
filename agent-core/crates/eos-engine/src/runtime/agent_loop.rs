//! The single "drive one agent" loop driver — the engine primitive
//! `run_agent`, relocated here from `eos-runtime` (advisor remediation
//! plan §2a).
//!
//! It wraps the engine's own `build_query_context` + `run_query` seam, so it is
//! an engine primitive, not a runtime concern. It takes explicit run handles
//! (constraint 6): the explicit run handles it needs ride in [`EngineRunHandles`],
//! which `eos-runtime` builds from its composition root and the advisor dispatch
//! path reads off [`QueryContext::run_handles`](crate::query::QueryContext).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use eos_agent_def::{AgentDefinition, AgentRegistry};
use eos_audit::{AuditEvent, AuditNode, AuditSink, AuditSource};
use eos_audit::{AGENT_RUN_COMPLETED, OS_RESOURCE_SAMPLED};
use eos_llm_client::{LlmClient, Message, ReasoningEffort, DEFAULT_MAX_TOKENS};
use eos_state::{AgentRunStore, ModelStore};
use eos_tools::{
    build_default_registry_with_services, AttemptSubmissionService, BackgroundSupervisorPort,
    CallerScope, CommandSessionSupervisorPort, ExecutionMetadata, RootSubmissionService,
    SandboxToolService, SkillToolService, ToolConfigSet, ToolRegistry, ToolResult,
    WorkflowControlPort,
};
use eos_types::{AgentRunId, JsonObject, SystemClock, TaskId};
use futures::StreamExt;
use serde_json::{json, Value};

use crate::agent::{build_query_context, BuildQueryContextInput};
use crate::notifications::NotificationService;
use crate::query::{run_query, EventSource, QueryExitReason};
use crate::telemetry::StreamEvent;

use super::resource_sample::capture_process_resource_sample;

/// Per-agent event-source factory seam (the Python `event_source_factory`).
///
/// `None` selects the live provider stream; the mock harness sets it so each
/// spawned agent runs the real loop against a scripted source. Owned here (next
/// to [`EventSource`]) so the engine-driven advisor run can resolve a source
/// without a runtime back-edge.
pub type EventSourceFactory = Arc<dyn Fn(&AgentDefinition) -> Arc<dyn EventSource> + Send + Sync>;

/// Per-run stream-event callback (the Python `AgentStreamEmitter`).
pub type EventCallback = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// Runtime-supplied extension point for non-core model tools, such as plugin
/// catalog tools. The engine owns per-agent registry construction but stays
/// ignorant of plugin catalog internals.
pub type ToolRegistryExtender = Arc<dyn Fn(&mut ToolRegistry) + Send + Sync>;

/// The explicit run handles `run_agent` needs, in place of a runtime-wide state bag
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

/// Inputs for [`run_agent`].
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
    /// Workflow control service for workflow tools and workflow-state hooks.
    pub workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    /// Background supervisor service for subagent/workflow tools and parent-exit
    /// cleanup.
    pub background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
    /// Command-session supervisor service for shell tools.
    pub command_session_supervisor: Option<Arc<dyn CommandSessionSupervisorPort>>,
    /// The per-request notification sink shared with the tools/heartbeat; the
    /// loop drains it each turn (anchor §7 instance identity). Helper
    /// runs (e.g. the advisor) pass a fresh, standalone service.
    pub notifier: NotificationService,
    /// Whether to record an `agent_run` row (create + finish).
    pub persist_agent_run: bool,
}

impl std::fmt::Debug for AgentRunInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunInput")
            .field("agent", &self.agent.name)
            .field("initial_messages", &self.initial_messages.len())
            .field("task_id", &self.task_id)
            .field("agent_run_id", &self.agent_run_id)
            .field("has_attempt_submission", &self.attempt_submission.is_some())
            .field("has_workflow_control", &self.workflow_control.is_some())
            .field(
                "has_background_supervisor",
                &self.background_supervisor.is_some(),
            )
            .field(
                "has_command_session_supervisor",
                &self.command_session_supervisor.is_some(),
            )
            .field("persist_agent_run", &self.persist_agent_run)
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

struct BackgroundRunFinalizer {
    supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    agent_run_ids: Vec<AgentRunId>,
    armed: bool,
}

impl BackgroundRunFinalizer {
    fn new(
        supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        agent_run_ids: Vec<AgentRunId>,
    ) -> Self {
        Self {
            supervisor,
            workflow_control,
            agent_run_ids,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BackgroundRunFinalizer {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(supervisor) = self.supervisor.take() else {
            return;
        };
        let workflow_control = self.workflow_control.take();
        let agent_run_ids = std::mem::take(&mut self.agent_run_ids);
        let reason = "engine run dropped before background finalization".to_owned();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("engine run dropped outside a Tokio runtime; background cleanup could not be spawned");
            return;
        };
        handle.spawn(async move {
            for agent_run_id in agent_run_ids {
                supervisor
                    .cancel_for_parent_exit(Some(&agent_run_id), workflow_control.clone(), &reason)
                    .await;
            }
        });
    }
}

/// Drive one agent to completion.
pub async fn run_agent(
    handles: &EngineRunHandles,
    input: AgentRunInput,
    on_event: Option<&EventCallback>,
) -> AgentRunResult {
    let run_started = Instant::now();
    let AgentRunInput {
        agent,
        mut initial_messages,
        task_id,
        agent_run_id,
        tool_metadata,
        attempt_submission,
        workflow_control,
        background_supervisor,
        command_session_supervisor,
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

    let (model, reasoning_effort) = match agent.model.clone() {
        Some(model) => (model, None),
        None => match handles.model_store.active().await.ok().flatten() {
            Some(registration) => (
                registration.model_key,
                reasoning_effort_from_kwargs(&registration.kwargs_json),
            ),
            None => ("default-model".to_owned(), None),
        },
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
        // The bound agent's own skill folder name scopes `load_skill_reference`
        // (Python `make_load_skill_reference_from_context`: `skill.parent.name`).
        skill_slug: agent
            .skill
            .as_deref()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned()),
    };
    let agent_run_ids: Vec<AgentRunId> = tool_metadata.agent_run_id.iter().cloned().collect();
    let registry = build_default_registry_with_services(
        &handles.tool_config,
        &caller_scope,
        handles.sandbox_service.clone(),
        handles.root_submission.clone(),
        attempt_submission,
        workflow_control.clone(),
        background_supervisor.clone(),
        command_session_supervisor,
        handles.skill_service.clone(),
    );
    let mut registry = registry;
    if let Some(extender) = &handles.tool_registry_extender {
        extender(&mut registry);
    }

    let ctx_result = build_query_context(BuildQueryContextInput {
        agent,
        model,
        client: Some(handles.llm_client.clone()),
        event_source,
        registry,
        base_system_prompt: String::new(),
        max_tokens: DEFAULT_MAX_TOKENS,
        reasoning_effort,
        cwd: PathBuf::from(&handles.workspace_root),
        agent_run_id: agent_run_id.clone(),
        task_id,
        tool_metadata,
        notifier,
        audit: Some(handles.audit.clone()),
        run_handles: Some(handles.clone()),
    });

    let mut ctx = match ctx_result {
        Ok(ctx) => ctx,
        Err(err) => {
            let summary = err.to_string();
            if persisted {
                finish_run(handles, &agent_run_id, Some(&summary)).await;
            }
            return AgentRunResult {
                terminal_result: None,
                error: Some(summary),
            };
        }
    };
    let mut background_finalizer = BackgroundRunFinalizer::new(
        background_supervisor.clone(),
        workflow_control.clone(),
        agent_run_ids.clone(),
    );

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
    finalize_background_for_agent(
        &ctx,
        error.as_deref(),
        background_supervisor,
        workflow_control,
        &agent_run_ids,
    )
    .await;
    background_finalizer.disarm();

    let terminal_result = ctx.terminal_result.clone();
    publish_agent_run_completed(
        handles,
        &ctx,
        run_started.elapsed().as_secs_f64() * 1000.0,
        error.as_deref(),
    );
    publish_os_resource_sampled(handles, &ctx);
    if persisted {
        finish_run(handles, &agent_run_id, error.as_deref()).await;
    }
    AgentRunResult {
        terminal_result,
        error,
    }
}

fn reasoning_effort_from_kwargs(kwargs_json: &str) -> Option<ReasoningEffort> {
    let value = serde_json::from_str::<Value>(kwargs_json).ok()?;
    let object = value.as_object()?;
    let raw = object
        .get("reasoning_effort")
        .or_else(|| object.get("effort"))
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("reasoning")
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str)
        })?;
    parse_reasoning_effort(raw)
}

fn parse_reasoning_effort(raw: &str) -> Option<ReasoningEffort> {
    match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "minimal" => Some(ReasoningEffort::Minimal),
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        _ => None,
    }
}

fn publish_agent_run_completed(
    handles: &EngineRunHandles,
    ctx: &crate::query::QueryContext,
    duration_ms: f64,
    error: Option<&str>,
) {
    let mut section = JsonObject::new();
    section.insert("duration_ms".to_owned(), json!(duration_ms));
    section.insert(
        "status".to_owned(),
        json!(if error.is_some() { "error" } else { "ok" }),
    );
    section.insert(
        "exit_reason".to_owned(),
        json!(ctx.exit_reason.map(exit_reason_value)),
    );
    if let Some(error) = error {
        section.insert("error".to_owned(), json!(error));
    }

    let mut payload = JsonObject::new();
    payload.insert("agent_run".to_owned(), Value::Object(section));
    publish_audit_event(handles, ctx, AGENT_RUN_COMPLETED, payload);
}

fn publish_os_resource_sampled(handles: &EngineRunHandles, ctx: &crate::query::QueryContext) {
    if !handles.audit.enabled() {
        return;
    }
    let Some(sample) = capture_process_resource_sample() else {
        return;
    };

    let mut payload = JsonObject::new();
    payload.insert(
        "os_resource".to_owned(),
        Value::Object(sample.into_payload()),
    );
    publish_audit_event(handles, ctx, OS_RESOURCE_SAMPLED, payload);
}

/// Publish one engine audit event scoped to this agent run, warning if the obs
/// sink rejects it. Shared by the per-run completion and OS-resource publishers.
fn publish_audit_event(
    handles: &EngineRunHandles,
    ctx: &crate::query::QueryContext,
    event_type: &str,
    payload: JsonObject,
) {
    let event = AuditEvent::new(
        AuditSource::Engine,
        event_type,
        agent_run_audit_node(ctx),
        payload,
        &SystemClock,
    );
    if let Err(err) = handles.audit.publish(&event) {
        tracing::warn!(
            error = %err,
            agent_run_id = ctx.agent_run_id.as_str(),
            event_type,
            "obs publish failed"
        );
    }
}

fn agent_run_audit_node(ctx: &crate::query::QueryContext) -> AuditNode {
    let mut node = AuditNode::builder()
        .agent_name(ctx.agent_name.clone())
        .agent_run_id(ctx.agent_run_id.clone());
    if let Some(request_id) = &ctx.tool_metadata.request_id {
        node = node.request_id(request_id.clone());
    }
    if let Some(task_id) = ctx
        .task_id
        .clone()
        .or_else(|| ctx.tool_metadata.task_id.clone())
    {
        node = node.task_id(task_id);
    }
    if let Some(sandbox_id) = &ctx.tool_metadata.sandbox_id {
        node = node.sandbox_id(sandbox_id.clone());
    }
    node.build()
}

const fn exit_reason_value(reason: QueryExitReason) -> &'static str {
    match reason {
        QueryExitReason::ToolStop => "tool_stop",
        QueryExitReason::TerminalNotSubmitted => "terminal_not_submitted",
    }
}

async fn finalize_background_for_agent(
    ctx: &crate::query::QueryContext,
    error: Option<&str>,
    supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    agent_run_ids: &[AgentRunId],
) {
    let Some(supervisor) = supervisor else {
        return;
    };
    let reason = match (ctx.exit_reason, error) {
        (_, Some(error)) => format!("engine run failed: {error}"),
        (Some(QueryExitReason::TerminalNotSubmitted), None) => {
            "parent agent exited without submitting a terminal tool".to_owned()
        }
        (Some(QueryExitReason::ToolStop), None) => "parent agent submitted its terminal".to_owned(),
        (None, None) => "parent agent exited".to_owned(),
    };
    for agent_run_id in agent_run_ids {
        supervisor
            .cancel_for_parent_exit(Some(agent_run_id), workflow_control.clone(), &reason)
            .await;
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_reasoning_effort_from_model_kwargs() {
        assert_eq!(
            reasoning_effort_from_kwargs(&json!({"reasoning_effort": "medium"}).to_string()),
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            reasoning_effort_from_kwargs(&json!({"effort": "high"}).to_string()),
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            reasoning_effort_from_kwargs(&json!({"reasoning": {"effort": "low"}}).to_string()),
            Some(ReasoningEffort::Low)
        );
        assert_eq!(reasoning_effort_from_kwargs("{}"), None);
    }
}

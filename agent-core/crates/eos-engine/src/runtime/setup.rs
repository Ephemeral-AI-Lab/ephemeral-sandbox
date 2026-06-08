//! Per-agent setup before entering the query loop.

use std::path::PathBuf;
use std::sync::Arc;

use eos_agent_run::AgentRunApi;
use eos_agent_def::AgentDefinition;
use eos_llm_client::DEFAULT_MAX_TOKENS;
use eos_tools::{
    build_default_registry_with_services, AttemptSubmissionService, CallerScope,
    CommandSessionPort, ExecutionMetadata, SubagentSessionPort, WorkflowServicePort,
    WorkflowSessionPort,
};
use eos_types::{AgentRunId, TaskId};

use crate::agent::{build_query_context, BuildQueryContextInput};
use crate::background::{BackgroundSessionFinalizer, BackgroundTeardownPort};
use crate::notifications::NotificationService;
use crate::query::QueryContext;
use crate::EngineError;

use super::control::AgentRunCancellation;
use super::foreground::ForegroundExecutor;
use super::types::EngineRunHandles;

pub(super) struct AgentRunSetupInput {
    pub(super) agent: AgentDefinition,
    pub(super) task_id: Option<TaskId>,
    pub(super) agent_run_id: AgentRunId,
    pub(super) tool_metadata: ExecutionMetadata,
    pub(super) attempt_submission: Option<AttemptSubmissionService>,
    pub(super) agent_run_service: Option<Arc<dyn AgentRunApi>>,
    pub(super) subagent_sessions: Option<Arc<dyn SubagentSessionPort>>,
    pub(super) workflow_service: Option<Arc<dyn WorkflowServicePort>>,
    pub(super) workflow_sessions: Option<Arc<dyn WorkflowSessionPort>>,
    pub(super) background_session: Option<Arc<dyn BackgroundTeardownPort>>,
    pub(super) command_session_port: Option<Arc<dyn CommandSessionPort>>,
    pub(super) notifier: NotificationService,
    pub(super) cancellation: AgentRunCancellation,
    pub(super) foreground: Arc<ForegroundExecutor>,
}

pub(super) struct PreparedAgentRun {
    pub(super) ctx: QueryContext,
    pub(super) background_session_finalizer: BackgroundSessionFinalizer,
}

pub(super) fn prepare_agent_run_context(
    handles: &EngineRunHandles,
    input: AgentRunSetupInput,
) -> Result<PreparedAgentRun, EngineError> {
    let AgentRunSetupInput {
        agent,
        task_id,
        agent_run_id,
        tool_metadata,
        attempt_submission,
        agent_run_service,
        subagent_sessions,
        workflow_service,
        workflow_sessions,
        background_session,
        command_session_port,
        notifier,
        cancellation,
        foreground,
    } = input;

    let model = agent.model.clone().unwrap_or_default();
    let event_source = handles
        .event_source_factory
        .as_ref()
        .map(|factory| factory(&agent));
    let caller_scope = caller_scope_for(handles, &agent);
    let mut registry = build_default_registry_with_services(
        &handles.tool_config,
        &caller_scope,
        handles.sandbox_service.clone(),
        handles.root_submission.clone(),
        attempt_submission,
        agent_run_service,
        subagent_sessions,
        workflow_service,
        workflow_sessions,
        command_session_port,
        handles.skill_service.clone(),
    );
    if let Some(extender) = &handles.tool_registry_extender {
        extender(&mut registry);
    }

    let background_session_finalizer = BackgroundSessionFinalizer::new(background_session);
    let ctx = build_query_context(BuildQueryContextInput {
        agent,
        model,
        client: Some(handles.llm_client.clone()),
        event_source,
        registry,
        base_system_prompt: String::new(),
        max_tokens: DEFAULT_MAX_TOKENS,
        cwd: PathBuf::from(&handles.workspace_root),
        agent_run_id,
        task_id,
        tool_metadata,
        notifier,
        cancellation,
        foreground,
        audit: Some(handles.audit.clone()),
        run_handles: Some(handles.clone()),
    })?;

    Ok(PreparedAgentRun {
        ctx,
        background_session_finalizer,
    })
}

fn caller_scope_for(handles: &EngineRunHandles, agent: &AgentDefinition) -> CallerScope {
    CallerScope {
        dispatchable_subagents: handles
            .agent_registry
            .dispatchable_subagent_names()
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect(),
        // The bound agent's own skill folder name scopes `load_skill_reference`.
        skill_slug: agent
            .skill
            .as_deref()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned()),
    }
}

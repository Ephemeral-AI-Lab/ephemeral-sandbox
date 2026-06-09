//! Runner-owned conversion from agent definitions to loop requests.

use std::sync::Arc;

use eos_types::{
    AgentDefinition, AgentLoopMessage, AgentName as DefinitionAgentName, AgentRunApi,
    AgentRunError, AgentRunId, AgentRunRecordTarget, AgentType, CreatedTaskAgentRun, Message,
    MessageRole, ParentedAgentRunKind, SpawnAgentRequest, SpawnAgentTarget, StartAgentLoopRequest,
    TaskAgentRunKind, DEFAULT_MAX_TOKENS,
};

use crate::completion::spawn_forwarder;
use crate::persistence::create_agent_run;
use crate::service::AgentRunService;

pub(crate) async fn spawn_agent(
    service: &AgentRunService,
    request: SpawnAgentRequest,
) -> Result<AgentRunId, AgentRunError> {
    if request.initial_messages.is_empty() {
        return Err(AgentRunError::Internal(
            "agent launch requires at least one initial message".to_owned(),
        ));
    }
    let requested_agent_name = request.agent_name.as_str().to_owned();
    let agent_name = DefinitionAgentName::new(request.agent_name.as_str())
        .map_err(|_| AgentRunError::AgentNotRegistered(requested_agent_name.clone()))?;
    let Some(agent_def) = service.agent_registry.get(&agent_name) else {
        return Err(AgentRunError::AgentNotRegistered(requested_agent_name));
    };
    let expected = expected_agent_type(&request.target.task_agent_run_kind());
    if agent_def.agent_type != expected {
        return Err(AgentRunError::WrongAgentType {
            agent_name: requested_agent_name,
            expected: agent_type_value(expected),
            actual: agent_type_value(agent_def.agent_type),
        });
    }

    let agent_def = (**agent_def).clone();
    let agent_run_id = AgentRunId::new_v4();
    let created_run = create_task_agent_run(service, &request, &agent_run_id, &agent_name).await?;
    let compat_task_id = match &request.target {
        SpawnAgentTarget::Root { .. } | SpawnAgentTarget::Workflow { .. } => {
            Some(&created_run.task_id)
        }
        SpawnAgentTarget::Subagent { .. } | SpawnAgentTarget::Advisor { .. } => None,
    };
    create_agent_run(
        &*service.agent_run_store,
        compat_task_id,
        &agent_run_id,
        agent_def.name.as_str(),
    )
    .await?;
    let record_target = created_run.record_target.clone();
    let start_request = build_start_agent_loop_request(&agent_def, request, record_target);
    let agent_run_api: Arc<dyn AgentRunApi> = Arc::new(service.clone());
    let started = service
        .loop_launcher
        .start_agent_loop(start_request, agent_run_api);

    service
        .active_agent_runs
        .insert(agent_run_id.clone(), started.cancellation)
        .await;
    spawn_forwarder(service.clone(), agent_run_id.clone(), started.completion);

    Ok(agent_run_id)
}

async fn create_task_agent_run(
    service: &AgentRunService,
    request: &SpawnAgentRequest,
    agent_run_id: &AgentRunId,
    agent_name: &DefinitionAgentName,
) -> Result<CreatedTaskAgentRun, AgentRunError> {
    match &request.target {
        SpawnAgentTarget::Root { request_id } => {
            service
                .task_agent_run_store
                .create_root_task_agent_run(request_id, agent_run_id, agent_name)
                .await
        }
        SpawnAgentTarget::Workflow {
            request_id,
            workflow,
            workflow_node_id,
        } => {
            service
                .task_agent_run_store
                .create_workflow_task_agent_run(
                    request_id,
                    agent_run_id,
                    workflow,
                    workflow_node_id,
                    agent_name,
                )
                .await
        }
        SpawnAgentTarget::Subagent { parent } => {
            service
                .task_agent_run_store
                .create_parented_task_agent_run(
                    agent_run_id,
                    parent,
                    ParentedAgentRunKind::Subagent,
                    request.tool_use_id.as_ref(),
                    agent_name,
                )
                .await
        }
        SpawnAgentTarget::Advisor { parent } => {
            service
                .task_agent_run_store
                .create_parented_task_agent_run(
                    agent_run_id,
                    parent,
                    ParentedAgentRunKind::Advisor,
                    request.tool_use_id.as_ref(),
                    agent_name,
                )
                .await
        }
    }
    .map_err(|err| AgentRunError::Internal(err.to_string()))
}

/// Build the thin engine loop request for one resolved agent.
#[must_use]
pub(crate) fn build_start_agent_loop_request(
    agent: &AgentDefinition,
    request: SpawnAgentRequest,
    record_target: AgentRunRecordTarget,
) -> StartAgentLoopRequest {
    StartAgentLoopRequest {
        record_target,
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

const fn agent_type_value(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Agent => "agent",
        AgentType::Subagent => "subagent",
        AgentType::Advisor => "advisor",
    }
}

pub(crate) const fn expected_agent_type(run_kind: &TaskAgentRunKind) -> AgentType {
    match run_kind {
        TaskAgentRunKind::Root | TaskAgentRunKind::Workflow { .. } => AgentType::Agent,
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Subagent,
            ..
        } => AgentType::Subagent,
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Advisor,
            ..
        } => AgentType::Advisor,
    }
}

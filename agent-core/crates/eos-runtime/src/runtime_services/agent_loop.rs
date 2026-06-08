//! Runtime implementations for engine agent-loop composition contracts.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_agent_ports::{
    AgentExecutionMetadataService, AgentLoopLauncher, AgentPortError, AgentRunApi, AgentState,
    AuditNodeBuildInput, ExecutionMetadataBuildInput,
};
use eos_audit::AuditNode;
use eos_engine::{
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, ProviderEventSource,
    TokioAgentLoopLauncher,
};
use eos_tool_ports::{ExecutionMetadata, ToolRegistry};
use eos_tools::{
    build_default_registry_with_services, AttemptSubmissionService, CallerScope,
    RootSubmissionService, SandboxToolService, SkillToolService,
};
use eos_types::WorkflowApi;

use super::RuntimeServices;
use crate::plugin_tools::register_plugin_tools;

/// Shared cell used to break the runner -> launcher -> tools -> runner cycle.
pub(crate) type AgentRunApiCell = Arc<OnceLock<Arc<dyn AgentRunApi>>>;

/// Build a production agent-loop launcher plus the cell that must be filled with
/// the lifecycle service after it is constructed.
pub(crate) fn build_agent_loop_launcher(
    services: RuntimeServices,
    attempt_submission: Option<AttemptSubmissionService>,
    workflow_service: Arc<OnceLock<Arc<dyn WorkflowApi>>>,
) -> (Arc<dyn AgentLoopLauncher>, AgentRunApiCell) {
    let agent_run_api = Arc::new(OnceLock::new());
    let metadata_service = Arc::new(RuntimeExecutionMetadataService::new(services.clone()));
    let registry_factory = Arc::new(RuntimeToolRegistryFactory {
        services: services.clone(),
        attempt_submission,
        workflow_service,
        agent_run_api: agent_run_api.clone(),
    });
    let launcher: Arc<dyn AgentLoopLauncher> = match services.engine.event_source_factory.clone() {
        Some(factory) => Arc::new(TokioAgentLoopLauncher::with_event_source_factory(
            factory,
            registry_factory,
            metadata_service,
        )),
        None => Arc::new(TokioAgentLoopLauncher::new(
            Arc::new(ProviderEventSource::new(services.engine.llm_client.clone())),
            registry_factory,
            metadata_service,
        )),
    };
    (launcher, agent_run_api)
}

struct RuntimeToolRegistryFactory {
    services: RuntimeServices,
    attempt_submission: Option<AttemptSubmissionService>,
    workflow_service: Arc<OnceLock<Arc<dyn WorkflowApi>>>,
    agent_run_api: AgentRunApiCell,
}

impl std::fmt::Debug for RuntimeToolRegistryFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeToolRegistryFactory")
            .finish_non_exhaustive()
    }
}

impl AgentLoopToolRegistryFactory for RuntimeToolRegistryFactory {
    fn build_tool_registry(
        &self,
        input: AgentLoopToolRegistryBuildInput,
    ) -> Result<ToolRegistry, eos_engine::EngineError> {
        let sandbox_service = SandboxToolService::new(self.services.sandbox.transport.clone());
        let plugin_sandbox_service = sandbox_service.clone();
        let caller = CallerScope {
            dispatchable_subagents: self
                .services
                .agent_core
                .agent_registry
                .dispatchable_subagent_names()
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect(),
            skill_slug: None,
        };
        let mut registry = build_default_registry_with_services(
            &self.services.agent_core.tool_config,
            &caller,
            sandbox_service,
            Some(RootSubmissionService::new(
                self.services.db.task_store.clone(),
                self.services.db.request_store.clone(),
            )),
            self.attempt_submission.clone(),
            self.agent_run_api.get().cloned(),
            Some(input.subagent_sessions),
            self.workflow_service.get().cloned(),
            Some(input.workflow_sessions),
            Some(input.command_sessions),
            SkillToolService::new(self.services.agent_core.skill_registry.clone()),
        );
        register_plugin_tools(&mut registry, &plugin_sandbox_service);
        Ok(registry)
    }
}

#[derive(Clone)]
struct RuntimeExecutionMetadataService {
    services: RuntimeServices,
}

impl RuntimeExecutionMetadataService {
    fn new(services: RuntimeServices) -> Self {
        Self { services }
    }

    async fn load_agent_state(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<AgentState, AgentPortError> {
        let run = self
            .services
            .db
            .agent_run_store
            .get(agent_run_id)
            .await
            .map_err(|err| AgentPortError::Internal(err.to_string()))?
            .ok_or_else(|| AgentPortError::Internal(format!("agent run {agent_run_id} missing")))?;
        let task = match &run.task_id {
            Some(task_id) => self
                .services
                .db
                .task_store
                .get(task_id)
                .await
                .map_err(|err| AgentPortError::Internal(err.to_string()))?,
            None => None,
        };
        let request = match task.as_ref() {
            Some(task) => self
                .services
                .db
                .request_store
                .get(&task.request_id)
                .await
                .map_err(|err| AgentPortError::Internal(err.to_string()))?,
            None => None,
        };

        Ok(AgentState {
            agent_run_id: agent_run_id.clone(),
            agent_name: run.agent_name,
            request_id: task.as_ref().map(|task| task.request_id.clone()),
            task_id: run.task_id,
            workflow_id: task.as_ref().and_then(|task| task.workflow_id.clone()),
            iteration_id: task.as_ref().and_then(|task| task.iteration_id.clone()),
            attempt_id: task.as_ref().and_then(|task| task.attempt_id.clone()),
            sandbox_id: request
                .as_ref()
                .and_then(|request| request.sandbox_id.clone()),
            workspace_root: request.map_or_else(String::new, |request| request.cwd),
            is_isolated_workspace_mode: false,
        })
    }
}

#[async_trait]
impl AgentExecutionMetadataService for RuntimeExecutionMetadataService {
    async fn agent_state(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<AgentState, AgentPortError> {
        self.load_agent_state(agent_run_id).await
    }

    async fn build_execution_metadata(
        &self,
        input: ExecutionMetadataBuildInput,
    ) -> Result<ExecutionMetadata, AgentPortError> {
        let state = self.load_agent_state(&input.agent_run_id).await?;
        Ok(ExecutionMetadata {
            agent_name: state.agent_name,
            agent_run_id: Some(state.agent_run_id),
            request_id: state.request_id,
            task_id: state.task_id,
            attempt_id: state.attempt_id,
            workflow_id: state.workflow_id,
            tool_use_id: Some(input.tool_use_id),
            sandbox_invocation_id: None,
            sandbox_id: state.sandbox_id,
            is_isolated_workspace_mode: state.is_isolated_workspace_mode,
            workspace_root: state.workspace_root,
            conversation: input.conversation,
        })
    }

    async fn build_audit_node(
        &self,
        input: AuditNodeBuildInput,
    ) -> Result<AuditNode, AgentPortError> {
        let state = self.load_agent_state(&input.agent_run_id).await?;
        let mut builder = AuditNode::builder()
            .agent_run_id(state.agent_run_id)
            .agent_name(state.agent_name);
        if let Some(request_id) = state.request_id {
            builder = builder.request_id(request_id);
        }
        if let Some(task_id) = state.task_id {
            builder = builder.task_id(task_id);
        }
        if let Some(workflow_id) = state.workflow_id {
            builder = builder.workflow_id(workflow_id);
        }
        if let Some(iteration_id) = state.iteration_id {
            builder = builder.iteration_id(iteration_id);
        }
        if let Some(attempt_id) = state.attempt_id {
            builder = builder.attempt_id(attempt_id);
        }
        if let Some(sandbox_id) = state.sandbox_id {
            builder = builder.sandbox_id(sandbox_id);
        }
        if let Some(tool_name) = input.tool_name {
            builder = builder.tool_name(tool_name.as_str());
        }
        if let Some(tool_use_id) = input.tool_use_id {
            builder = builder.tool_use_id(tool_use_id);
        }
        Ok(builder.build())
    }
}

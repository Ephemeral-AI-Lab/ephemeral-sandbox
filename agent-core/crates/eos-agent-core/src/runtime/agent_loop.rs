//! Runtime implementations for engine agent-loop composition contracts.

use std::sync::Arc;

use async_trait::async_trait;
use eos_engine::{
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, BackgroundSessionRuntimeFactory,
    EngineError, EngineEventOutputs, EngineEventSink, ExecutionMetadataBuildInput,
    LlmProviderStreamSource, TokioAgentLoopLauncher, ToolCallHookStores,
    ToolExecutionMetadataReader,
};
use eos_sandbox_port::SandboxCommandService;
use eos_tool::{
    build_default_registry, CallerScope, ExecutionMetadata, TerminalSubmissionRuntime,
    ToolRegistry, ToolRuntime,
};
use eos_types::{
    AgentLoopLauncher, AgentRunRuntimeSnapshot, WorkflowApi, WorkflowAttemptSubmissionApi,
};

use super::plugins::register_plugin_tools;
use super::AgentCoreRuntime;

/// Build a production agent-loop launcher from concrete runtime contracts.
pub(crate) fn build_agent_loop_launcher(
    services: &AgentCoreRuntime,
    attempt_submission: Arc<dyn WorkflowAttemptSubmissionApi>,
    workflow_service: Arc<dyn WorkflowApi>,
    live_event_sink: Option<EngineEventSink>,
) -> Arc<dyn AgentLoopLauncher> {
    let execution_metadata_reader =
        Arc::new(RuntimeToolExecutionMetadataReader::new(services.clone()));
    let registry_factory = Arc::new(RuntimeToolRegistryFactory {
        services: services.clone(),
        attempt_submission,
        workflow_service,
    });
    let background_sessions = BackgroundSessionRuntimeFactory::new(
        Arc::new(SandboxCommandService::new(
            services.sandbox.transport.clone(),
        )),
        services.engine.command_session_completion_poll_interval(),
        registry_factory.workflow_service.clone(),
    );
    let hook_stores = ToolCallHookStores::new(
        services.db.task_store.clone(),
        services.db.agent_run_store.clone(),
        services.db.workflow_store.clone(),
    );
    let launcher_impl = match services.engine.provider_stream_source_factory.clone() {
        Some(factory) => TokioAgentLoopLauncher::with_provider_stream_source_factory(
            factory,
            registry_factory.clone(),
            execution_metadata_reader.clone(),
        ),
        None => TokioAgentLoopLauncher::new(
            Arc::new(LlmProviderStreamSource::new(
                services.engine.llm_client.clone(),
            )),
            registry_factory,
            execution_metadata_reader,
        ),
    }
    .with_background_sessions(background_sessions)
    .with_tool_call_hook_stores(hook_stores)
    .with_event_outputs(
        EngineEventOutputs::new()
            .with_live_event_sink(live_event_sink)
            .with_run_record_writer(services.records.run_record_writer.clone()),
    );
    Arc::new(launcher_impl)
}

struct RuntimeToolRegistryFactory {
    services: AgentCoreRuntime,
    attempt_submission: Arc<dyn WorkflowAttemptSubmissionApi>,
    workflow_service: Arc<dyn WorkflowApi>,
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
        let background = input.background.ok_or_else(|| {
            eos_engine::EngineError::Internal(
                "background session runtime not initialized".to_owned(),
            )
        })?;
        let runtime = ToolRuntime {
            sandbox: self.services.sandbox.transport.clone(),
            workflow: self.workflow_service.clone(),
            launcher: input.agent_run_api.clone(),
            skills: self.services.agent_core.skill_registry.clone(),
            submission: TerminalSubmissionRuntime::new(
                self.services.db.task_store.clone(),
                self.services.db.request_store.clone(),
                self.attempt_submission.clone(),
            ),
            background: Arc::new(background),
            workspace_mode: Arc::new(self.services.agent_state.clone()),
        };
        let mut registry =
            build_default_registry(&self.services.agent_core.tool_config, &caller, &runtime);
        register_plugin_tools(&mut registry, &self.services.sandbox.transport);
        Ok(registry)
    }
}

#[derive(Clone)]
struct RuntimeToolExecutionMetadataReader {
    services: AgentCoreRuntime,
}

impl RuntimeToolExecutionMetadataReader {
    fn new(services: AgentCoreRuntime) -> Self {
        Self { services }
    }

    async fn load_agent_run_snapshot(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<AgentRunRuntimeSnapshot, EngineError> {
        let runtime_state = self.services.agent_state.get(agent_run_id);
        let run = self
            .services
            .db
            .agent_run_store
            .get(agent_run_id)
            .await
            .map_err(|err| EngineError::Internal(err.to_string()))?;
        let agent_name = run
            .as_ref()
            .map(|run| run.agent_name.clone())
            .or_else(|| runtime_state.as_ref().map(|state| state.agent_name.clone()))
            .ok_or_else(|| EngineError::Internal(format!("agent run {agent_run_id} missing")))?;
        let task_id = run
            .as_ref()
            .and_then(|run| run.task_id.clone())
            .or_else(|| {
                runtime_state
                    .as_ref()
                    .and_then(|state| state.task_id.clone())
            });
        let task = match &task_id {
            Some(task_id) => self
                .services
                .db
                .task_store
                .get(task_id)
                .await
                .map_err(|err| EngineError::Internal(err.to_string()))?,
            None => None,
        };
        let request_id = task
            .as_ref()
            .map(|task| task.request_id.clone())
            .or_else(|| {
                runtime_state
                    .as_ref()
                    .and_then(|state| state.request_id.clone())
            });
        let request = match &request_id {
            Some(request_id) => self
                .services
                .db
                .request_store
                .get(request_id)
                .await
                .map_err(|err| EngineError::Internal(err.to_string()))?,
            None => None,
        };
        let runtime_workspace_root = runtime_state
            .as_ref()
            .map(|state| state.workspace_root.as_str())
            .filter(|workspace_root| !workspace_root.trim().is_empty());

        Ok(AgentRunRuntimeSnapshot {
            agent_run_id: agent_run_id.clone(),
            agent_name,
            request_id,
            task_id,
            workflow_id: task
                .as_ref()
                .and_then(|task| task.workflow_id.clone())
                .or_else(|| {
                    runtime_state
                        .as_ref()
                        .and_then(|state| state.workflow_id.clone())
                }),
            iteration_id: task
                .as_ref()
                .and_then(|task| task.iteration_id.clone())
                .or_else(|| {
                    runtime_state
                        .as_ref()
                        .and_then(|state| state.iteration_id.clone())
                }),
            attempt_id: task
                .as_ref()
                .and_then(|task| task.attempt_id.clone())
                .or_else(|| {
                    runtime_state
                        .as_ref()
                        .and_then(|state| state.attempt_id.clone())
                }),
            sandbox_id: runtime_state
                .as_ref()
                .and_then(|state| state.sandbox_id.clone())
                .or_else(|| {
                    request
                        .as_ref()
                        .and_then(|request| request.sandbox_id.clone())
                }),
            workspace_root: runtime_workspace_root.map_or_else(
                || request.map_or_else(String::new, |request| request.cwd),
                str::to_owned,
            ),
            is_isolated_workspace_mode: runtime_state
                .as_ref()
                .is_some_and(|state| state.is_isolated_workspace_mode),
        })
    }
}

#[async_trait]
impl ToolExecutionMetadataReader for RuntimeToolExecutionMetadataReader {
    async fn agent_run_snapshot(
        &self,
        agent_run_id: &eos_types::AgentRunId,
    ) -> Result<AgentRunRuntimeSnapshot, EngineError> {
        self.load_agent_run_snapshot(agent_run_id).await
    }

    async fn build_execution_metadata(
        &self,
        input: ExecutionMetadataBuildInput,
    ) -> Result<ExecutionMetadata, EngineError> {
        let state = self.load_agent_run_snapshot(&input.agent_run_id).await?;
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
}

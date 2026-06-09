//! Runtime-only agent state used for per-tool metadata rendering.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use eos_agent_run::AgentRuntimeStateStore;
use eos_tool::{IsolatedWorkspaceModeControl, ToolError};
use eos_types::{
    AgentRunError, AgentRunId, AttemptId, CreatedTaskAgentRun, IterationId, RequestId, SandboxId,
    SpawnAgentRequest, TaskId, WorkflowId,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeAgentStateService {
    inner: Arc<RwLock<HashMap<AgentRunId, RuntimeAgentState>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeAgentState {
    pub(crate) agent_name: String,
    pub(crate) request_id: Option<RequestId>,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) workflow_id: Option<WorkflowId>,
    pub(crate) iteration_id: Option<IterationId>,
    pub(crate) attempt_id: Option<AttemptId>,
    pub(crate) sandbox_id: Option<SandboxId>,
    pub(crate) workspace_root: String,
    pub(crate) is_isolated_workspace_mode: bool,
}

impl RuntimeAgentStateService {
    pub(crate) fn record_spawn_request(
        &self,
        request: &SpawnAgentRequest,
        created_run: &CreatedTaskAgentRun,
    ) -> Result<(), AgentRunError> {
        let mut states = self
            .inner
            .write()
            .map_err(|_| AgentRunError::Internal("runtime agent state lock poisoned".to_owned()))?;
        states.insert(
            created_run.agent_run_id.clone(),
            RuntimeAgentState::from_spawn(request, created_run),
        );
        Ok(())
    }

    pub(crate) fn remove(&self, agent_run_id: &AgentRunId) {
        if let Ok(mut states) = self.inner.write() {
            states.remove(agent_run_id);
        }
    }

    pub(crate) fn get(&self, agent_run_id: &AgentRunId) -> Option<RuntimeAgentState> {
        self.inner
            .read()
            .ok()
            .and_then(|states| states.get(agent_run_id).cloned())
    }

    fn set_isolated_workspace_mode(
        &self,
        agent_run_id: &AgentRunId,
        is_isolated: bool,
    ) -> Result<(), ToolError> {
        let mut states = self
            .inner
            .write()
            .map_err(|_| ToolError::Internal("runtime agent state lock poisoned".to_owned()))?;
        let Some(state) = states.get_mut(agent_run_id) else {
            return Err(ToolError::Internal(format!(
                "runtime agent state missing for {agent_run_id}"
            )));
        };
        state.is_isolated_workspace_mode = is_isolated;
        Ok(())
    }
}

impl AgentRuntimeStateStore for RuntimeAgentStateService {
    fn record_spawn_request(
        &self,
        request: &SpawnAgentRequest,
        created_run: &CreatedTaskAgentRun,
    ) -> Result<(), AgentRunError> {
        self.record_spawn_request(request, created_run)
    }

    fn remove_runtime_state(&self, agent_run_id: &AgentRunId) {
        self.remove(agent_run_id);
    }
}

#[async_trait]
impl IsolatedWorkspaceModeControl for RuntimeAgentStateService {
    async fn set_isolated_workspace_mode(
        &self,
        agent_run_id: AgentRunId,
        is_isolated: bool,
    ) -> Result<(), ToolError> {
        self.set_isolated_workspace_mode(&agent_run_id, is_isolated)
    }
}

impl RuntimeAgentState {
    fn from_spawn(request: &SpawnAgentRequest, created_run: &CreatedTaskAgentRun) -> Self {
        let workflow = request.target.workflow();
        Self {
            agent_name: request.agent_name.as_str().to_owned(),
            request_id: Some(request.target.request_id().clone()),
            task_id: Some(created_run.task_id.clone()),
            workflow_id: workflow.map(|coords| coords.workflow_id.clone()),
            iteration_id: workflow.map(|coords| coords.iteration_id.clone()),
            attempt_id: workflow.map(|coords| coords.attempt_id.clone()),
            sandbox_id: request.sandbox_id.clone(),
            workspace_root: request.workspace_root.clone(),
            is_isolated_workspace_mode: request.is_isolated_workspace_mode,
        }
    }
}

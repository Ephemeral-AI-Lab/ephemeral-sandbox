//! Concrete backend-facing agent-core service.

use std::sync::Arc;

use eos_agent_run::AgentRunService;
use eos_sandbox_port::SandboxGateway;
use eos_types::{
    AgentName, AttemptStore, IterationStore, RequestId, RequestStore, TaskAgentRunStore, TaskRun,
    WorkflowStore,
};

use crate::dto::{
    CancelUserRequestInput, CancelUserRequestOutput, CreateUserRequestInput,
    CreateUserRequestOutput, UserRequestDetail, UserRequestSummary,
};
use crate::error::AgentCoreServerError;

/// Backend-facing request service.
#[derive(Clone)]
pub struct AgentCoreService {
    pub(crate) request_store: Arc<dyn RequestStore>,
    pub(crate) task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    pub(crate) workflow_store: Arc<dyn WorkflowStore>,
    pub(crate) iteration_store: Arc<dyn IterationStore>,
    pub(crate) attempt_store: Arc<dyn AttemptStore>,
    pub(crate) agent_run_service: AgentRunService,
    pub(crate) sandbox_gateway: Arc<dyn SandboxGateway>,
    pub(crate) settings: AgentCoreServiceSettings,
}

impl std::fmt::Debug for AgentCoreService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentCoreService")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// Fixed settings for [`AgentCoreService`].
#[derive(Debug, Clone)]
pub struct AgentCoreServiceSettings {
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Root agent profile name.
    pub root_agent_name: AgentName,
}

/// Constructor dependencies for [`AgentCoreService`].
pub struct AgentCoreServiceDependencies {
    /// Durable top-level request rows.
    pub request_store: Arc<dyn RequestStore>,
    /// Durable task-agent-run lineage rows.
    pub task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    /// Workflow rows.
    pub workflow_store: Arc<dyn WorkflowStore>,
    /// Iteration rows.
    pub iteration_store: Arc<dyn IterationStore>,
    /// Attempt rows.
    pub attempt_store: Arc<dyn AttemptStore>,
    /// Active and durable agent-run lifecycle.
    pub agent_run_service: AgentRunService,
    /// Sandbox binding plus sandbox tool transport.
    pub sandbox_gateway: Arc<dyn SandboxGateway>,
    /// Fixed service settings.
    pub settings: AgentCoreServiceSettings,
}

impl std::fmt::Debug for AgentCoreServiceDependencies {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentCoreServiceDependencies")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl AgentCoreService {
    /// Build a service from explicit dependencies.
    #[must_use]
    pub fn new(dependencies: AgentCoreServiceDependencies) -> Self {
        Self {
            request_store: dependencies.request_store,
            task_agent_run_store: dependencies.task_agent_run_store,
            workflow_store: dependencies.workflow_store,
            iteration_store: dependencies.iteration_store,
            attempt_store: dependencies.attempt_store,
            agent_run_service: dependencies.agent_run_service,
            sandbox_gateway: dependencies.sandbox_gateway,
            settings: dependencies.settings,
        }
    }

    /// Create a user request and start its root agent run.
    ///
    /// # Errors
    /// Returns [`AgentCoreServerError`] when provisioning, persistence, or
    /// spawning fails.
    pub async fn create_user_request(
        &self,
        input: CreateUserRequestInput,
    ) -> Result<CreateUserRequestOutput, AgentCoreServerError> {
        crate::user_request::create::create_user_request(self, input).await
    }

    /// Cancel one user request.
    ///
    /// # Errors
    /// Returns [`AgentCoreServerError`] when the request is absent, already
    /// terminal, or cancellation persistence fails.
    pub async fn cancel_user_request(
        &self,
        input: CancelUserRequestInput,
    ) -> Result<CancelUserRequestOutput, AgentCoreServerError> {
        crate::user_request::cancel::cancel_user_request(self, input).await
    }

    /// Read one user request.
    ///
    /// # Errors
    /// Returns [`AgentCoreServerError`] on store failure.
    pub async fn read_user_request(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<UserRequestDetail>, AgentCoreServerError> {
        crate::user_request::query::read_user_request(self, request_id).await
    }

    /// List user request summaries.
    ///
    /// # Errors
    /// Returns [`AgentCoreServerError`] on store failure.
    pub async fn list_user_requests(
        &self,
    ) -> Result<Vec<UserRequestSummary>, AgentCoreServerError> {
        crate::user_request::query::list_user_requests(self).await
    }

    /// List task-agent-runs for a user request.
    ///
    /// # Errors
    /// Returns [`AgentCoreServerError`] on store failure.
    pub async fn list_user_request_tasks(
        &self,
        request_id: &RequestId,
    ) -> Result<Vec<TaskRun>, AgentCoreServerError> {
        crate::user_request::query::list_user_request_tasks(self, request_id).await
    }
}

//! Request-scoped runtime input.

use eos_config::WorkflowConfig;
use eos_types::RequestId;

/// Values that vary per top-level request.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RequestRunInput {
    /// Caller-minted request id.
    pub request_id: RequestId,
    /// Root prompt.
    pub prompt: String,
    /// Optional explicit sandbox id requested by the caller.
    pub sandbox_id: Option<String>,
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Workflow runtime config for this request.
    pub workflow_config: WorkflowConfig,
}

impl RequestRunInput {
    /// Build request-scoped input.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        prompt: impl Into<String>,
        workspace_root: impl Into<String>,
        workflow_config: WorkflowConfig,
    ) -> Self {
        Self {
            request_id,
            prompt: prompt.into(),
            sandbox_id: None,
            workspace_root: workspace_root.into(),
            workflow_config,
        }
    }

    /// Set an explicit sandbox id to start/bind.
    #[must_use]
    pub fn with_sandbox_id(mut self, sandbox_id: impl Into<String>) -> Self {
        self.sandbox_id = Some(sandbox_id.into());
        self
    }
}

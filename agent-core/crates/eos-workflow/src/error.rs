use eos_state::CoreError;

/// Result alias for workflow operations.
pub type Result<T> = std::result::Result<T, WorkflowError>;

/// Workflow lifecycle and context-builder invariant failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowError {
    /// A delegated workflow prompt was empty after trimming.
    #[error("workflow prompt must be nonblank")]
    BlankPrompt,
    /// A required entity was not found in the store.
    #[error("{entity} {id:?} not found")]
    NotFound {
        /// Entity kind.
        entity: &'static str,
        /// Entity id.
        id: String,
    },
    /// A lifecycle invariant was violated.
    #[error("{0}")]
    Invariant(String),
    /// Context recipe and scope do not line up.
    #[error("{0}")]
    Recipe(String),
    /// A context scope omitted a field required by the selected role.
    #[error("context scope is missing required field: {0}")]
    MissingContextField(&'static str),
    /// An agent definition was missing or invalid for launch.
    #[error("{0}")]
    AgentDefinition(String),
    /// Store failure propagated from an upstream store trait.
    #[error("{0}")]
    Store(#[from] CoreError),
    /// JSON encoding/decoding failure at the iteration/workflow outcomes boundary.
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    /// Tool framework fault while adapting a downstream-state port.
    #[error("{0}")]
    Tool(#[from] eos_tools::ToolError),
    /// A spawned agent task panicked or was cancelled.
    #[error("agent task join failed: {0}")]
    Join(String),
}

impl WorkflowError {
    pub(crate) fn invariant(message: impl Into<String>) -> Self {
        Self::Invariant(message.into())
    }

    pub(crate) fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            id: id.into(),
        }
    }
}

impl From<eos_agent_def::AgentDefError> for WorkflowError {
    fn from(value: eos_agent_def::AgentDefError) -> Self {
        Self::AgentDefinition(value.to_string())
    }
}

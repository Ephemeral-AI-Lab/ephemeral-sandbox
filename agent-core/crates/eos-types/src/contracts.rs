//! Cross-crate lifecycle contracts.
//!
//! This module holds owner-neutral behavior traits and passive DTOs that are shared
//! across sibling crates. Keeping them in `eos-types` avoids dependency cycles:
//! engine, workflow, tools, and agent-run can all consume the contracts without
//! depending on each other's concrete implementations.

use async_trait::async_trait;

use std::collections::BTreeMap;

use crate::{
    AgentName, AgentRunId, AttemptId, CoreError, GeneratorSubmission, IterationId, JsonObject,
    Message, PlanDisposition, PlanNodeId, ReducerSubmission, RequestId, SandboxId, TaskId,
    WorkflowId,
};

/// Request to spawn any agent kind.
#[derive(Debug, Clone)]
pub struct SpawnAgentRequest {
    /// Agent profile name to launch.
    pub agent_name: AgentName,
    /// Optional caller-provided run id; one is minted when absent.
    pub agent_run_id: Option<AgentRunId>,
    /// Initial transcript.
    pub initial_messages: Vec<Message>,
    /// Closed spawn target and lineage facts.
    pub target: SpawnAgentTarget,
    /// Bound sandbox id.
    pub sandbox_id: Option<SandboxId>,
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Whether the caller is in isolated-workspace mode.
    pub is_isolated_workspace_mode: bool,
    /// Whether to persist the run row.
    pub persist: bool,
}

/// Current runtime metadata facts for one agent run.
#[derive(Debug, Clone)]
pub struct AgentState {
    /// Agent-run id.
    pub agent_run_id: AgentRunId,
    /// Bound agent profile name.
    pub agent_name: String,
    /// Owning request id.
    pub request_id: Option<RequestId>,
    /// Owning task id.
    pub task_id: Option<TaskId>,
    /// Owning workflow id.
    pub workflow_id: Option<WorkflowId>,
    /// Owning workflow iteration id.
    pub iteration_id: Option<IterationId>,
    /// Owning attempt id.
    pub attempt_id: Option<AttemptId>,
    /// Bound sandbox id.
    pub sandbox_id: Option<SandboxId>,
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Whether the run currently has an isolated workspace open.
    pub is_isolated_workspace_mode: bool,
}

/// Workflow coordinates used by workflow task-agent-runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCoordinates {
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Owning iteration id.
    pub iteration_id: IterationId,
    /// Owning attempt id.
    pub attempt_id: AttemptId,
}

/// Parent-launched task-agent-run kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentedAgentRunKind {
    /// Background subagent run.
    Subagent,
    /// Blocking advisor run.
    Advisor,
}

/// Parent run anchor for a parent-launched agent run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentAgentRunAnchor {
    /// Owning request id.
    pub request_id: RequestId,
    /// Parent task id.
    pub parent_task_id: TaskId,
    /// Parent agent-run id.
    pub agent_run_id: AgentRunId,
}

/// Closed spawn target for agent-run creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnAgentTarget {
    /// Root request agent.
    Root {
        /// Owning request id.
        request_id: RequestId,
        /// Existing root task id until root row creation moves fully into agent-run.
        task_id: TaskId,
    },
    /// Workflow planner/generator/reducer task agent.
    Workflow {
        /// Owning request id.
        request_id: RequestId,
        /// Existing workflow task id until workflow row creation moves fully into agent-run.
        task_id: TaskId,
        /// Owning workflow coordinates.
        workflow: WorkflowCoordinates,
        /// Workflow task role.
        role: WorkflowTaskRole,
    },
    /// Parent-launched subagent run.
    Subagent {
        /// Parent run anchor.
        parent: ParentAgentRunAnchor,
    },
    /// Parent-launched advisor run.
    Advisor {
        /// Parent run anchor.
        parent: ParentAgentRunAnchor,
    },
}

impl SpawnAgentTarget {
    /// Owning request id.
    #[must_use]
    pub const fn request_id(&self) -> &RequestId {
        match self {
            Self::Root { request_id, .. } | Self::Workflow { request_id, .. } => request_id,
            Self::Subagent { parent } | Self::Advisor { parent } => &parent.request_id,
        }
    }

    /// Current run task id when the transitional caller still supplies one.
    #[must_use]
    pub const fn current_task_id(&self) -> Option<&TaskId> {
        match self {
            Self::Root { task_id, .. } | Self::Workflow { task_id, .. } => Some(task_id),
            Self::Subagent { .. } | Self::Advisor { .. } => None,
        }
    }

    /// Workflow coordinates for workflow task-agent-runs.
    #[must_use]
    pub const fn workflow(&self) -> Option<&WorkflowCoordinates> {
        match self {
            Self::Workflow { workflow, .. } => Some(workflow),
            Self::Root { .. } | Self::Subagent { .. } | Self::Advisor { .. } => None,
        }
    }

    /// Convert the spawn target into the current task-agent-run classification.
    #[must_use]
    pub fn task_agent_run_kind(&self) -> TaskAgentRunKind {
        match self {
            Self::Root { .. } => TaskAgentRunKind::Root,
            Self::Workflow { workflow, role, .. } => TaskAgentRunKind::Workflow {
                workflow: workflow.clone(),
                role: *role,
            },
            Self::Subagent { parent } => TaskAgentRunKind::Parented {
                parent_agent_run_id: parent.agent_run_id.clone(),
                kind: ParentedAgentRunKind::Subagent,
            },
            Self::Advisor { parent } => TaskAgentRunKind::Parented {
                parent_agent_run_id: parent.agent_run_id.clone(),
                kind: ParentedAgentRunKind::Advisor,
            },
        }
    }
}

/// Closed task-agent-run layout choice used to derive the current record path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAgentRunKind {
    /// Root request agent.
    Root,
    /// Delegated workflow planner/generator/reducer task agent.
    Workflow {
        /// Owning workflow coordinates.
        workflow: WorkflowCoordinates,
        /// Workflow task role.
        role: WorkflowTaskRole,
    },
    /// Parent-launched task-agent-run under a parent agent.
    Parented {
        /// Parent agent-run id.
        parent_agent_run_id: AgentRunId,
        /// Parent-launched run kind.
        kind: ParentedAgentRunKind,
    },
}

/// Workflow task role used for task-agent-run path labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTaskRole {
    /// Planner task.
    Planner,
    /// Generator task.
    Generator,
    /// Reducer task.
    Reducer,
}

/// Terminal outcome for one agent run.
#[derive(Debug, Clone)]
pub struct AgentRunOutcome {
    /// Agent-run id.
    pub agent_run_id: AgentRunId,
    /// Terminal status.
    pub status: AgentRunStatus,
    /// Persisted submission payload, when available.
    pub submission_payload: Option<JsonObject>,
    /// Final message history, when the runner makes it available.
    pub message_history: Vec<Message>,
    /// Provider token count, when known.
    pub token_count: Option<i64>,
    /// Framework/engine error summary, when the run failed.
    pub error: Option<String>,
}

/// Agent-run terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunStatus {
    /// The run completed normally.
    Completed,
    /// The run failed.
    Failed,
    /// The run was cancelled.
    Cancelled,
}

/// Error returned by the agent-run lifecycle API.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentRunError {
    /// The requested agent run is not active in this process and has no durable
    /// terminal outcome available.
    #[error("agent run {0} is not active in this process")]
    NotActiveInProcess(AgentRunId),

    /// The requested agent name was not registered.
    #[error("agent {0:?} is not registered")]
    AgentNotRegistered(String),

    /// The requested agent exists but is not launchable for this operation.
    #[error("agent {agent_name:?} is not a {expected} agent (actual: {actual})")]
    WrongAgentType {
        /// Requested agent name.
        agent_name: String,
        /// Expected type label.
        expected: &'static str,
        /// Actual type label.
        actual: &'static str,
    },

    /// Recursive subagent launch is disallowed.
    #[error("subagents may not spawn further subagents")]
    RecursiveSubagent,

    /// Waiting failed because the completion channel closed.
    #[error("agent run completion channel closed for {0}")]
    CompletionChannelClosed(AgentRunId),

    /// A store, engine, or framework operation failed.
    #[error("agent run failed: {0}")]
    Internal(String),
}

/// Lifecycle API for spawning, waiting, polling, and cancelling agent runs.
#[async_trait]
pub trait AgentRunApi: Send + Sync {
    /// Spawn an agent and return its natural run id immediately.
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> Result<AgentRunId, AgentRunError>;

    /// Wait for one agent run to publish a terminal outcome.
    async fn wait_for_agent_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError>;

    /// Nonblocking terminal outcome poll for background managers.
    async fn poll_agent_run_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError>;

    /// Cancel one active agent run.
    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), AgentRunError>;
}

/// One planner-authored generator task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    /// Caller-assigned task id.
    pub id: PlanNodeId,
    /// Bound subagent profile name.
    pub agent_name: String,
    /// Ids this task depends on.
    pub needs: Vec<PlanNodeId>,
}

/// One planner-authored reducer task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReducer {
    /// Caller-assigned reducer id.
    pub id: PlanNodeId,
    /// Ids this reducer depends on.
    pub needs: Vec<PlanNodeId>,
    /// The reducer's instruction prompt.
    pub prompt: String,
}

/// A validated planner DAG submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerPlan {
    /// Owning attempt.
    pub attempt_id: AttemptId,
    /// The planner's own task.
    pub planner_task_id: TaskId,
    /// Whether the plan completes the attempt or defers a goal.
    pub disposition: PlanDisposition,
    /// The generator tasks, in submission order.
    pub tasks: Vec<PlanTask>,
    /// Per-task instruction specs, keyed by task id.
    pub task_specs: BTreeMap<PlanNodeId, String>,
    /// The reducer tasks, in submission order.
    pub reducers: Vec<PlanReducer>,
}

/// The result of applying a terminal submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionAck {
    /// The submission was accepted by the orchestrator.
    Accepted,
    /// The submission was rejected with a model-facing message.
    Rejected(String),
}

/// Per-attempt submission application for terminal tools.
#[async_trait]
pub trait WorkflowAttemptSubmissionApi: Send + Sync {
    /// Apply a validated planner DAG.
    async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, CoreError>;

    /// Record one generator task's terminal outcome.
    async fn submit_generator(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<SubmissionAck, CoreError>;

    /// Record one reducer task's terminal outcome.
    async fn apply_reducer(
        &self,
        submission: ReducerSubmission,
    ) -> Result<SubmissionAck, CoreError>;
}

/// Error returned by recursive request/workflow cancellation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CancelError {
    /// An upstream store operation failed.
    #[error("store error: {0}")]
    Store(#[from] CoreError),
    /// A lifecycle invariant broke or an internal operation failed.
    #[error("{0}")]
    Internal(String),
}

/// Recursive agent-core cancellation primitives.
#[async_trait]
pub trait AgentCoreCancellationApi: Send + Sync {
    /// Cancel a persisted task and any live run bound to it.
    async fn cancel_task(&self, task_id: &TaskId, reason: &str) -> Result<(), CancelError>;

    /// Cancel a live agent run.
    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), CancelError>;
}

/// Request to start a delegated workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartWorkflowRequest {
    /// Parent task launching the workflow.
    pub parent_task_id: TaskId,
    /// Agent run that owns the launch.
    pub agent_run_id: AgentRunId,
    /// Delegated workflow goal.
    pub workflow_goal: String,
}

/// A started delegated workflow, keyed by its natural [`WorkflowId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedWorkflow {
    /// The persisted workflow id.
    pub workflow_id: WorkflowId,
    /// The delegated goal, retained for background-session display.
    pub workflow_goal: String,
}

/// Terminal status for a delegated workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTerminalStatus {
    /// The workflow succeeded.
    Completed,
    /// The workflow failed.
    Failed,
    /// The workflow was cancelled.
    Cancelled,
}

/// Terminal workflow facts for background accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalWorkflow {
    /// The persisted workflow id.
    pub workflow_id: WorkflowId,
    /// Terminal status.
    pub status: WorkflowTerminalStatus,
}

/// One outstanding workflow launched by a parent task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingWorkflow {
    /// The persisted workflow id.
    pub workflow_id: WorkflowId,
    /// The workflow goal.
    pub workflow_goal: String,
}

/// Error returned by the delegated-workflow API. Tool callers map this onto
/// their own framework-fault enum at the tool boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowApiError {
    /// An upstream store operation failed.
    #[error("store error: {0}")]
    Store(#[from] CoreError),
    /// A lifecycle invariant broke or an internal operation failed.
    #[error("{0}")]
    Internal(String),
}

/// Delegated-workflow lifecycle operations used by the model-facing workflow
/// tools and the engine background workflow manager.
#[async_trait]
pub trait WorkflowApi: Send + Sync {
    /// Start a delegated workflow.
    async fn start_workflow(
        &self,
        request: StartWorkflowRequest,
    ) -> Result<StartedWorkflow, WorkflowApiError>;

    /// Render workflow status for the model-facing check tool.
    async fn check_workflow_status(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<String, WorkflowApiError>;

    /// Cancel a workflow by its natural id, returning a model-facing message.
    async fn cancel_workflow(
        &self,
        workflow_id: &WorkflowId,
        reason: &str,
    ) -> Result<String, WorkflowApiError>;

    /// Poll terminal workflow state for background accounting.
    async fn poll_terminal_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<TerminalWorkflow>, WorkflowApiError>;

    /// All workflows this parent task still has outstanding for `agent_run_id`.
    async fn find_outstanding_workflows(
        &self,
        parent_task_id: &TaskId,
        agent_run_id: &AgentRunId,
    ) -> Result<Vec<OutstandingWorkflow>, WorkflowApiError>;

    /// The delegation-ancestry depth of `workflow_id` (1 = top-level).
    async fn workflow_depth(&self, workflow_id: &WorkflowId) -> Result<u32, WorkflowApiError>;
}

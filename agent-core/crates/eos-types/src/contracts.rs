//! Cross-crate lifecycle contracts.
//!
//! These modules hold owner-neutral behavior traits and passive DTOs shared
//! across sibling crates. Keeping them in `eos-types` avoids dependency cycles:
//! engine, workflow, tools, and agent-run can all consume the contracts without
//! depending on each other's concrete implementations.

mod agent_run {
    //! Agent-run lifecycle launch contracts.

    use async_trait::async_trait;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use crate::{
        AgentName, AgentRunId, AttemptId, IterationId, JsonObject, Message, RequestId, SandboxId,
        TaskId, ToolUseId, WorkflowId,
    };

    use super::record::{
        ParentedAgentRunKind, TaskAgentRunKind, WorkflowCoordinates, WorkflowNodeId,
    };

    /// Request to spawn any agent kind.
    #[derive(Debug, Clone)]
    pub struct SpawnAgentRequest {
        /// Agent profile name to launch.
        pub agent_name: AgentName,
        /// Initial transcript.
        pub initial_messages: Vec<Message>,
        /// Closed spawn target and lineage facts.
        pub target: SpawnAgentTarget,
        /// Durable launch fact for workflow/parented launches. It is never part of
        /// the record-dir target.
        pub tool_use_id: Option<ToolUseId>,
        /// Bound sandbox id.
        pub sandbox_id: Option<SandboxId>,
        /// Request-visible workspace root.
        pub workspace_root: String,
        /// Whether the caller is in isolated-workspace mode.
        pub is_isolated_workspace_mode: bool,
    }

    /// Runtime-only metadata snapshot for one agent run.
    #[derive(Debug, Clone)]
    pub struct AgentRunRuntimeSnapshot {
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

    /// Parent run anchor for a parent-launched agent run.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    pub struct ParentAgentRunAnchor {
        /// Owning request id.
        pub request_id: RequestId,
        /// Parent task id.
        pub parent_task_id: TaskId,
        /// Parent agent-run id.
        pub agent_run_id: AgentRunId,
    }

    /// Closed spawn target for agent-run creation.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    pub enum SpawnAgentTarget {
        /// Root request agent.
        Root {
            /// Owning request id.
            request_id: RequestId,
        },
        /// Workflow planner/generator/reducer task agent.
        Workflow {
            /// Owning request id.
            request_id: RequestId,
            /// Owning workflow coordinates.
            workflow: WorkflowCoordinates,
            /// Workflow node id, including the planner/generator/reducer role.
            workflow_node_id: WorkflowNodeId,
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
                Self::Root { request_id } | Self::Workflow { request_id, .. } => request_id,
                Self::Subagent { parent } | Self::Advisor { parent } => &parent.request_id,
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
                Self::Workflow {
                    workflow,
                    workflow_node_id,
                    ..
                } => TaskAgentRunKind::Workflow {
                    workflow: workflow.clone(),
                    role: workflow_node_id.role(),
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
        async fn spawn_agent(
            &self,
            request: SpawnAgentRequest,
        ) -> Result<AgentRunId, AgentRunError>;

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
}
mod cancellation {
    //! Agent-core cancellation contracts.

    use async_trait::async_trait;

    use crate::{AgentRunId, CoreError, TaskId};

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
}
mod record {
    //! Execution-lineage record contracts.

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use crate::{
        AgentRunId, AttemptId, GeneratorId, IterationId, PlannerId, ReducerId, RequestId, TaskId,
        WorkflowId,
    };

    /// Workflow coordinates used by workflow task-agent-runs.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    pub struct WorkflowCoordinates {
        /// Owning workflow id.
        pub workflow_id: WorkflowId,
        /// Owning iteration id.
        pub iteration_id: IterationId,
        /// Owning attempt id.
        pub attempt_id: AttemptId,
    }

    /// Parent-launched task-agent-run kind.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum ParentedAgentRunKind {
        /// Background subagent run.
        Subagent,
        /// Blocking advisor run.
        Advisor,
    }

    /// Closed task-agent-run layout choice used to derive the current record path.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum WorkflowTaskRole {
        /// Planner task.
        Planner,
        /// Generator task.
        Generator,
        /// Reducer task.
        Reducer,
    }

    impl WorkflowTaskRole {
        /// The canonical record/task path label.
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Planner => "planner",
                Self::Generator => "generator",
                Self::Reducer => "reducer",
            }
        }

        /// The task path segment prefix for this workflow role.
        #[must_use]
        pub const fn task_segment_prefix(self) -> &'static str {
            match self {
                Self::Planner => "planner-task",
                Self::Generator => "generator-task",
                Self::Reducer => "reducer-task",
            }
        }
    }

    /// Workflow node identity for planner/generator/reducer task-agent-runs.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum WorkflowNodeId {
        /// Attempt-level planner node.
        Planner {
            /// Workflow-local planner id.
            planner_id: PlannerId,
        },
        /// Planner-authored generator node.
        Generator {
            /// Workflow-local generator id from the planner-authored DAG.
            generator_id: GeneratorId,
        },
        /// Planner-authored reducer node.
        Reducer {
            /// Workflow-local reducer id from the planner-authored DAG.
            reducer_id: ReducerId,
        },
    }

    impl WorkflowNodeId {
        /// Workflow task role represented by this node id.
        #[must_use]
        pub const fn role(&self) -> WorkflowTaskRole {
            match self {
                Self::Planner { .. } => WorkflowTaskRole::Planner,
                Self::Generator { .. } => WorkflowTaskRole::Generator,
                Self::Reducer { .. } => WorkflowTaskRole::Reducer,
            }
        }

        /// Workflow-local role id as a borrowed string.
        #[must_use]
        pub fn role_id(&self) -> &str {
            match self {
                Self::Planner { planner_id } => planner_id.as_str(),
                Self::Generator { generator_id } => generator_id.as_str(),
                Self::Reducer { reducer_id } => reducer_id.as_str(),
            }
        }
    }

    impl ParentedAgentRunKind {
        /// The canonical row value.
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Subagent => "subagent",
                Self::Advisor => "advisor",
            }
        }

        /// The request-rooted child directory segment.
        #[must_use]
        pub const fn collection_segment(self) -> &'static str {
            match self {
                Self::Subagent => "subagents",
                Self::Advisor => "advisors",
            }
        }

        /// The run path segment prefix.
        #[must_use]
        pub const fn run_segment_prefix(self) -> &'static str {
            match self {
                Self::Subagent => "subagent-run",
                Self::Advisor => "advisor-run",
            }
        }
    }

    /// Input to record-dir resolution for a task-backed agent run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AgentRunRecordIndex {
        /// Owning request.
        pub request_id: RequestId,
        /// Agent-run id.
        pub agent_run_id: AgentRunId,
        /// The run's own task id.
        pub task_id: TaskId,
        /// Closed lineage kind.
        pub kind: TaskAgentRunKind,
        /// Resolved parent record directory for parent-launched runs.
        ///
        /// This is populated by the durable lineage query before formatting. Spawn
        /// classification can still use [`TaskAgentRunKind`] without knowing paths.
        pub parent_record_dir: Option<AgentRunRecordDir>,
    }

    /// Request-rooted record directory for one agent run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AgentRunRecordDir(String);

    impl AgentRunRecordDir {
        /// Construct from a normalized request-rooted path string.
        #[must_use]
        pub fn new(value: impl Into<String>) -> Self {
            Self(value.into())
        }

        /// Borrow the path string.
        #[must_use]
        pub fn as_str(&self) -> &str {
            &self.0
        }

        /// Consume and return the path string.
        #[must_use]
        pub fn into_string(self) -> String {
            self.0
        }
    }

    impl std::fmt::Display for AgentRunRecordDir {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(f)
        }
    }

    /// Passive engine-facing record target.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AgentRunRecordTarget {
        /// Owning request.
        pub request_id: RequestId,
        /// Agent-run id.
        pub agent_run_id: AgentRunId,
        /// The run's own task id.
        pub task_id: TaskId,
        /// Closed lineage kind used by the engine record writer.
        pub task_agent_run_kind: TaskAgentRunKind,
        /// Resolved request-rooted record directory.
        pub record_dir: AgentRunRecordDir,
    }

    /// Row-creation-local task-agent-run result.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CreatedTaskAgentRun {
        /// Agent-run id.
        pub agent_run_id: AgentRunId,
        /// The run's own task id.
        pub task_id: TaskId,
        /// Pre-resolved record target for the engine loop.
        pub record_target: AgentRunRecordTarget,
    }

    /// Flat read-side child index for one task-backed run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TaskExecutionIndex {
        /// The task id being indexed.
        pub task_id: TaskId,
        /// Its main agent-run id.
        pub agent_run_id: AgentRunId,
        /// Workflows launched by this task.
        pub workflow_ids: Vec<WorkflowId>,
        /// Parent-launched subagent run ids.
        pub subagent_ids: Vec<AgentRunId>,
        /// Parent-launched advisor run ids.
        pub advisor_ids: Vec<AgentRunId>,
    }

    /// Format a request-rooted record directory from a resolved record index.
    ///
    /// The formatter is intentionally pure and owns the path-segment literals.
    #[must_use]
    pub fn format_record_dir(index: &AgentRunRecordIndex) -> AgentRunRecordDir {
        let request_root = format!("requests/{}", index.request_id.as_str());
        let agent_run_segment = prefixed("agent-run", index.agent_run_id.as_str());
        let task_id = index.task_id.as_str();
        let dir = match &index.kind {
            TaskAgentRunKind::Root => format!(
                "{}/{}/{}",
                request_root,
                prefixed("root-task", task_id),
                agent_run_segment
            ),
            TaskAgentRunKind::Workflow { workflow, role } => format!(
                "{}/workflows/{}/{}/{}/{}/{}",
                request_root,
                prefixed("workflow", workflow.workflow_id.as_str()),
                prefixed("iteration", workflow.iteration_id.as_str()),
                prefixed("attempt", workflow.attempt_id.as_str()),
                prefixed(role.task_segment_prefix(), task_id),
                agent_run_segment
            ),
            TaskAgentRunKind::Parented { kind, .. } => {
                let parent_root = index
                    .parent_record_dir
                    .as_ref()
                    .map_or(request_root.as_str(), AgentRunRecordDir::as_str);
                format!(
                    "{}/{}/{}",
                    parent_root,
                    kind.collection_segment(),
                    prefixed(kind.run_segment_prefix(), index.agent_run_id.as_str())
                )
            }
        };
        AgentRunRecordDir::new(dir)
    }

    fn prefixed(prefix: &str, id: &str) -> String {
        format!("{prefix}-{id}")
    }
}
mod workflow {
    //! Workflow and terminal-submission contracts.

    use std::collections::BTreeMap;

    use async_trait::async_trait;

    use crate::{
        AgentRunId, AttemptId, CoreError, GeneratorId, GeneratorSubmission, PlanDisposition,
        ReducerId, ReducerSubmission, TaskId, ToolUseId, WorkflowId,
    };

    /// One planner-authored generator task.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PlanTask {
        /// Caller-assigned generator id.
        pub generator_id: GeneratorId,
        /// Bound subagent profile name.
        pub agent_name: String,
        /// Generator ids this task depends on.
        pub needs: Vec<GeneratorId>,
    }

    /// One planner-authored reducer task.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PlanReducer {
        /// Caller-assigned reducer id.
        pub reducer_id: ReducerId,
        /// Generator ids this reducer depends on.
        pub needs: Vec<GeneratorId>,
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
        /// Per-generator instruction specs, keyed by generator id.
        pub task_specs: BTreeMap<GeneratorId, String>,
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

    /// Request to start a delegated workflow.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StartWorkflowRequest {
        /// Parent task launching the workflow.
        pub parent_task_id: TaskId,
        /// Agent run that owns the launch.
        pub agent_run_id: AgentRunId,
        /// Tool use that requested the workflow, if available.
        pub tool_use_id: Option<ToolUseId>,
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

    /// One open delegated workflow launched by an agent run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct OpenDelegatedWorkflow {
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

        /// List open delegated workflows launched by this agent run.
        async fn list_open_delegated_workflows_for_agent_run(
            &self,
            agent_run_id: &AgentRunId,
        ) -> Result<Vec<OpenDelegatedWorkflow>, WorkflowApiError>;

        /// The delegation-ancestry depth of `workflow_id` (1 = top-level).
        async fn workflow_depth(&self, workflow_id: &WorkflowId) -> Result<u32, WorkflowApiError>;
    }
}

pub use agent_run::{
    AgentRunApi, AgentRunError, AgentRunOutcome, AgentRunRuntimeSnapshot, AgentRunStatus,
    ParentAgentRunAnchor, SpawnAgentRequest, SpawnAgentTarget,
};
pub use cancellation::{AgentCoreCancellationApi, CancelError};
pub use record::{
    format_record_dir, AgentRunRecordDir, AgentRunRecordIndex, AgentRunRecordTarget,
    CreatedTaskAgentRun, ParentedAgentRunKind, TaskAgentRunKind, TaskExecutionIndex,
    WorkflowCoordinates, WorkflowNodeId, WorkflowTaskRole,
};
pub use workflow::{
    OpenDelegatedWorkflow, PlanReducer, PlanTask, PlannerPlan, StartWorkflowRequest,
    StartedWorkflow, SubmissionAck, TerminalWorkflow, WorkflowApi, WorkflowApiError,
    WorkflowAttemptSubmissionApi, WorkflowTerminalStatus,
};

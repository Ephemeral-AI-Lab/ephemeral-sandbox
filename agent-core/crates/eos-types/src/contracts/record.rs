//! Execution-lineage record contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentRunId, AttemptId, IterationId, RequestId, TaskId, WorkflowId};

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
    /// Delegated workflow planner/worker task agent.
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
    /// Worker task.
    Worker,
}

impl WorkflowTaskRole {
    /// The canonical record/task path label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Worker => "worker",
        }
    }

    /// The task path segment prefix for this workflow role.
    #[must_use]
    pub const fn task_segment_prefix(self) -> &'static str {
        match self {
            Self::Planner => "planner-task",
            Self::Worker => "worker-task",
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

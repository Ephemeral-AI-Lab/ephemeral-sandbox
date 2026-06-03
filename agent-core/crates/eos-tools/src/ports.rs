//! The six narrow downstream-state **port traits** (anchor §6b), owned here and
//! implemented downstream, injected at the composition root.
//!
//! These satisfy DIP for tools that need engine/workflow/host state without a
//! backward DAG edge (`eos-tools` is upstream of `eos-engine`/`eos-workflow`).
//! Each is `#[async_trait]` (stored behind `Arc<dyn _>` in [`ExecutionMetadata`])
//! and **sealed** (`api-sealed-trait`) via the [`Sealed`] friend-marker so only
//! agent-core crates implement them. Each has exactly one wired implementor
//! (ISP), recorded on the anchor §6 SOLID Seam Map.
//!
//! Port methods return `Result<_, ToolError>`: an `Err` is a genuine framework
//! fault (the implementor's own wiring/transport break); an in-band, model-facing
//! "not found"/"rejected" outcome is carried in the `Ok` value (a rendered
//! `String` or a typed outcome), which the tool wraps into a [`ToolResult`].

use std::collections::BTreeMap;

use async_trait::async_trait;
use eos_state::{GeneratorSubmission, PlannerKind, ReducerSubmission};
use eos_types::{SandboxId, SubagentSessionId, TaskId, WorkflowId, WorkflowSessionId};
use serde::Serialize;
use serde_json::Value;

use crate::error::ToolError;
use crate::metadata::ExecutionMetadata;
use crate::result::ToolResult;

/// Friend-seal for the port traits (`api-sealed-trait`).
///
/// `#[doc(hidden)] pub` rather than crate-private because the wired implementors
/// live in separate downstream crates (`eos-engine`/`eos-workflow`/`eos-runtime`)
/// and in-crate `#[cfg(test)]` fakes; a strictly-private marker would be
/// unreachable to them. External (non-agent-core) crates must not implement the
/// ports. Mirrors `eos_state::Sealed`.
#[doc(hidden)]
pub trait Sealed {}

// ---------------------------------------------------------------------------
// WorkflowControlPort — delegate / check / cancel workflow.
// ---------------------------------------------------------------------------

/// A started delegated workflow handle (returned by [`WorkflowControlPort::start`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedWorkflow {
    /// The persisted workflow id.
    pub workflow_id: WorkflowId,
    /// The agent-facing background handle (`wf_<n>`, Python `workflow_task_id`).
    pub workflow_task_id: WorkflowSessionId,
}

/// One outstanding workflow launched by a parent task (for `find_outstanding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingWorkflow {
    /// The persisted workflow id.
    pub workflow_id: WorkflowId,
    /// The agent-facing background handle.
    pub workflow_task_id: WorkflowSessionId,
    /// The workflow goal.
    pub workflow_goal: String,
}

/// Per-Attempt workflow control for the `delegate`/`check`/`cancel_workflow`
/// tools. Implemented by the `eos-workflow` + `eos-engine` workflow-handle
/// adapter. The live workflow/outcome state lives downstream, so `status`/
/// `cancel` return already-rendered, model-facing text.
#[async_trait]
pub trait WorkflowControlPort: Sealed + Send + Sync {
    /// Launch a delegated workflow from a running parent task; the parent keeps
    /// running (no synthetic root workflow).
    async fn start(
        &self,
        parent_task_id: &TaskId,
        agent_id: &str,
        workflow_goal: &str,
    ) -> Result<StartedWorkflow, ToolError>;

    /// Render delegated-workflow progress (and terminal outcomes when available).
    async fn status(
        &self,
        workflow_id: &WorkflowId,
        workflow_task_id: Option<&WorkflowSessionId>,
    ) -> Result<String, ToolError>;

    /// Cancel an outstanding delegated workflow by its background handle.
    async fn cancel(
        &self,
        workflow_task_id: &WorkflowSessionId,
        reason: &str,
    ) -> Result<String, ToolError>;

    /// All workflows this parent task still has outstanding for `agent_id`.
    async fn find_outstanding(
        &self,
        parent_task_id: &TaskId,
        agent_id: &str,
    ) -> Result<Vec<OutstandingWorkflow>, ToolError>;

    /// Whether `workflow_id` is itself a nested (delegated-within-a-workflow)
    /// workflow. Read by the `DisallowNestedPlannerDeferral` pre-hook (Python
    /// `is_nested_workflow`).
    async fn is_nested_workflow(&self, workflow_id: &WorkflowId) -> Result<bool, ToolError>;
}

// ---------------------------------------------------------------------------
// PlanSubmissionPort — planner / generator / reducer terminal submissions.
// ---------------------------------------------------------------------------

/// One planner-authored generator task (id + bound agent + `needs` edges).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    /// Caller-assigned task id (validated unique by the tool).
    pub id: String,
    /// Bound subagent profile name.
    pub agent_name: String,
    /// Ids this task depends on.
    pub needs: Vec<String>,
}

/// One planner-authored reducer task (id + `needs` + prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReducer {
    /// Caller-assigned reducer id.
    pub id: String,
    /// Ids this reducer depends on.
    pub needs: Vec<String>,
    /// The reducer's instruction prompt.
    pub prompt: String,
}

/// A validated planner DAG submission.
///
/// Richer than `eos_state::PlannerSubmission` (which carries only resolved task
/// ids): the generator/reducer rows do not exist yet, so the implementor
/// (`eos-workflow` `AttemptOrchestrator`) creates the `Task` rows from this DAG,
/// builds the `eos-state` `PlannerSubmission`, and applies it. The **structural**
/// validation (duplicate ids, missing/extra `task_specs`, deferred-goal
/// nonblank-when-present) is done by the tool before this DTO is built
/// (AC-tools-12), so the port receives a well-formed DAG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerPlan {
    /// Owning attempt (from execution context).
    pub attempt_id: eos_types::AttemptId,
    /// The planner's own task (from execution context).
    pub planner_task_id: TaskId,
    /// Whether the plan completes the attempt or defers a goal.
    pub kind: PlannerKind,
    /// Goal carried to the next iteration, normalized (nonblank) when present.
    pub deferred_goal_for_next_iteration: Option<String>,
    /// The generator tasks, in submission order.
    pub tasks: Vec<PlanTask>,
    /// Per-task instruction specs, keyed by task id.
    pub task_specs: BTreeMap<String, String>,
    /// The reducer tasks, in submission order.
    pub reducers: Vec<PlanReducer>,
}

/// The result of applying a terminal submission: accepted, or rejected with a
/// model-facing message (the Python `AttemptSubmissionContextError` /
/// `WorkflowInvariantViolation` in-band path). `Err(ToolError)` stays reserved
/// for genuine framework faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionAck {
    /// The submission was accepted by the orchestrator.
    Accepted,
    /// The submission was rejected; the message is shown to the model in-band.
    Rejected(String),
}

/// Per-Attempt submission application for the planner/generator/reducer terminal
/// tools. Implemented by the `eos-workflow` `AttemptOrchestrator`.
#[async_trait]
pub trait PlanSubmissionPort: Sealed + Send + Sync {
    /// Apply a validated planner DAG (`orchestrator.record_plan`, the
    /// non-advancing recording entry point). The implementor performs the
    /// downstream-state checks (planner-task ownership, unknown-agent, DAG cycle)
    /// and persists the task rows.
    async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, ToolError>;

    /// Record one generator task's terminal outcome.
    async fn submit_generator(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<SubmissionAck, ToolError>;

    /// Record one reducer task's terminal outcome (the attempt's exit gate).
    async fn apply_reducer(
        &self,
        submission: ReducerSubmission,
    ) -> Result<SubmissionAck, ToolError>;
}

// ---------------------------------------------------------------------------
// SubagentSupervisorPort — spawn / check / cancel subagent + background count.
// ---------------------------------------------------------------------------

/// A started subagent handle (returned on the `Launched` arm of [`SpawnedSubagent`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedSubagent {
    /// The agent-facing background handle (`subagent_<n>`).
    pub subagent_session_id: SubagentSessionId,
}

/// The outcome of [`SubagentSupervisorPort::spawn`]: a tracked launch, or an
/// in-band validation rejection rendered to the model. Mirrors [`SubmissionAck`]:
/// validation failures (recursion / unknown / non-subagent) are model-facing
/// `Ok(Rejected)` outcomes, not `Err(ToolError)` framework faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnedSubagent {
    /// The subagent run was launched and is tracked.
    Launched(StartedSubagent),
    /// Validation rejected the dispatch; the message is shown to the model.
    Rejected(String),
}

/// Per-agent, per-kind in-flight background-task count (Running records only),
/// scoped to one `agent_id`, serialized to JSON for the terminal-drain audit
/// assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BackgroundInflightReport {
    /// `subagent + workflow + command_session`.
    pub total: usize,
    /// In-flight subagent runs for this agent.
    pub subagent: usize,
    /// Outstanding delegated workflows for this agent. Workflow lifecycle is
    /// owned by the workflow lane (a sibling crate) with authoritative persisted
    /// state, so the supervisor does not track it: the supervisor leaves this `0`
    /// and the terminal hook populates the count from the authoritative
    /// [`WorkflowControlPort::find_outstanding`].
    pub workflow: usize,
    /// In-flight, supervisor-tracked command sessions for this agent
    /// (diagnostic; the authoritative live-session gate is the daemon RPC).
    pub command_session: usize,
}

/// The engine background supervisor, for the subagent tools and the
/// no-inflight-background-tasks hook. Implemented by `eos-engine`.
#[async_trait]
pub trait SubagentSupervisorPort: Sealed + Send + Sync {
    /// Validate, launch, and track a dispatchable subagent run. `ctx` is the
    /// caller's execution metadata: the implementor reads the caller identity
    /// from it (for recursion + the agent-scoped count) and clones it to build
    /// the child run's metadata. Validation failures (recursion / unknown /
    /// non-subagent) return `Ok(Rejected(_))`; `Err` is a framework fault only.
    async fn spawn(
        &self,
        ctx: &ExecutionMetadata,
        agent_name: &str,
        prompt: &str,
    ) -> Result<SpawnedSubagent, ToolError>;

    /// Render a tracked subagent's status/result as the model-facing
    /// [`ToolResult`] (the rendered JSON payload, or `is_error` for a missing
    /// session).
    async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
        last_n_messages: u8,
    ) -> Result<ToolResult, ToolError>;

    /// Cancel a tracked subagent session, returning the model-facing
    /// [`ToolResult`] (`is_error` for an unknown / already-settled session).
    async fn cancel(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> Result<ToolResult, ToolError>;

    /// This agent's in-flight background report (Running-only), without mutating
    /// state — the reject-mode read for `enter_isolated_workspace`.
    async fn inflight_report(&self, agent_id: &str) -> BackgroundInflightReport;

    /// Drain this agent's in-flight subagent runs (settle `Cancelled` + abort)
    /// and return the post-drain report — the drain-to-0 path the terminal /
    /// exit prehook runs so a live or phantom subagent never wedges the terminal.
    async fn drain_for_agent(&self, agent_id: &str) -> BackgroundInflightReport;
}

// ---------------------------------------------------------------------------
// CommandSessionSupervisorPort — register / recover / mark / count background
// PTY command sessions.
// ---------------------------------------------------------------------------

/// The engine background supervisor's command-session surface, used by the
/// `exec_command`/`write_stdin` tools to track sandbox-bound background command
/// sessions and to recover a terminal result across the heartbeat race
/// (anchor §5, §8). Implemented by `eos-engine` on the same supervisor instance
/// as [`SubagentSupervisorPort`].
///
/// The `result` payloads are the daemon completion's `result` map (status,
/// `exit_code`, `output.stdout`, …); they are opaque JSON to the supervisor and
/// rendered by the engine when delivered.
#[async_trait]
pub trait CommandSessionSupervisorPort: Sealed + Send + Sync {
    /// Register a freshly-started background command session as running. The
    /// `command_session_id` is the daemon-minted `cmd_<n>` correlation key.
    async fn register(
        &self,
        command_session_id: &str,
        sandbox_id: &str,
        agent_id: &str,
        command: &str,
    );

    /// The stored terminal result for a session whose live daemon session is
    /// already gone (the recover race), or `None` when it is still running or
    /// untracked.
    async fn command_session_result(&self, command_session_id: &str) -> Option<Value>;

    /// Mark a session reported (delivered) with the terminal `result` a control
    /// tool observed inline, so the heartbeat does not re-deliver it.
    async fn mark_command_session_reported(&self, command_session_id: &str, result: Value);

    /// Whether a session's completion was already delivered to the model (via the
    /// heartbeat). A late `write_stdin` poll uses this to return a terse
    /// already-reported note instead of re-dumping the completion (anchor §8/D8).
    async fn command_session_already_reported(&self, command_session_id: &str) -> bool;
}

// ---------------------------------------------------------------------------
// IsolatedWorkspacePort — enter / exit isolated workspace.
// ---------------------------------------------------------------------------

/// The `eos-runtime` adapter over the `eos-sandbox-host` isolated-workspace
/// lifecycle. The adapter enforces *no in-flight ephemeral jobs / command
/// sessions* before `enter`, and cancels/drains per-agent background work before
/// `exit`. Wired at the composition root (sandbox-host is upstream of
/// `eos-tools`, so no direct `eos-sandbox-host -> eos-tools` edge).
#[async_trait]
pub trait IsolatedWorkspacePort: Sealed + Send + Sync {
    /// Open this agent's private isolated workspace; returns model-facing text.
    async fn enter(
        &self,
        agent_id: &str,
        sandbox_id: &SandboxId,
        layer_stack_root: &str,
    ) -> Result<String, ToolError>;

    /// Close and discard this agent's isolated workspace; returns model-facing
    /// text.
    async fn exit(
        &self,
        agent_id: &str,
        sandbox_id: &SandboxId,
        grace_s: f64,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// NotificationSink — system notifications.
// ---------------------------------------------------------------------------

/// A system notification a tool/hook asks the engine to surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemNotification {
    /// The notification event key (e.g. `nested_planner_deferral_disabled`).
    pub event: String,
    /// Free-text body.
    pub message: String,
}

/// The engine notification service. Implemented by `eos-engine`.
#[async_trait]
pub trait NotificationSink: Sealed + Send + Sync {
    /// Surface one system notification.
    async fn notify_system(&self, notification: SystemNotification) -> Result<(), ToolError>;
}

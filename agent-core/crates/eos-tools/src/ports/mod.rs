//! The five narrow downstream-state **port traits** (anchor §6b), owned here and
//! implemented downstream, injected at the composition root.
//!
//! These satisfy DIP for tools that need engine/workflow/host state without a
//! backward DAG edge (`eos-tools` is upstream of `eos-engine`/`eos-workflow`).
//! Each is `#[async_trait]` and **sealed** (`api-sealed-trait`) via the
//! [`Sealed`] friend-marker so only
//! agent-core crates implement them. Each has exactly one wired implementor
//! (ISP), recorded on the anchor §6 SOLID Seam Map.
//!
//! Port methods return `Result<_, ToolError>`: an `Err` is a genuine framework
//! fault (the implementor's own wiring/transport break); an in-band, model-facing
//! "not found"/"rejected" outcome is carried in the `Ok` value (a rendered
//! `String` or a typed outcome), which the tool wraps into a [`ToolResult`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_state::{GeneratorSubmission, PlanDisposition, PlanNodeId, ReducerSubmission};
use eos_types::{
    AgentRunId, CommandSessionId, SandboxId, SubagentSessionId, TaskId, WorkflowId,
    WorkflowSessionId,
};
use serde::Serialize;
use serde_json::Value;

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::result::ToolResult;

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
pub struct StartedWorkflowHandle {
    /// The persisted workflow id.
    pub workflow_id: WorkflowId,
    /// The agent-facing background handle (`wf_<n>`, Rust `workflow_task_id`).
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
        agent_run_id: &AgentRunId,
        workflow_goal: &str,
    ) -> Result<StartedWorkflowHandle, ToolError>;

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

    /// All workflows this parent task still has outstanding for `agent_run_id`.
    async fn find_outstanding(
        &self,
        parent_task_id: &TaskId,
        agent_run_id: &AgentRunId,
    ) -> Result<Vec<OutstandingWorkflow>, ToolError>;

    /// The delegation-ancestry depth of `workflow_id` (1 = top-level, 2 = nested
    /// once, ...). Read by the `DisallowNestedPlannerDeferral` pre-hook to compare
    /// against its configured `max_depth` (Rust `workflow_depth`).
    async fn workflow_depth(&self, workflow_id: &WorkflowId) -> Result<u32, ToolError>;
}

// ---------------------------------------------------------------------------
// AttemptSubmissionPort — planner / generator / reducer terminal submissions.
// ---------------------------------------------------------------------------

/// One planner-authored generator task (id + bound agent + `needs` edges).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    /// Caller-assigned task id (validated unique by the tool).
    pub id: PlanNodeId,
    /// Bound subagent profile name.
    pub agent_name: String,
    /// Ids this task depends on.
    pub needs: Vec<PlanNodeId>,
}

/// One planner-authored reducer task (id + `needs` + prompt).
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
    pub disposition: PlanDisposition,
    /// The generator tasks, in submission order.
    pub tasks: Vec<PlanTask>,
    /// Per-task instruction specs, keyed by task id.
    pub task_specs: BTreeMap<PlanNodeId, String>,
    /// The reducer tasks, in submission order.
    pub reducers: Vec<PlanReducer>,
}

/// The result of applying a terminal submission: accepted, or rejected with a
/// model-facing message (the Rust `AttemptSubmissionContextError` /
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
pub trait AttemptSubmissionPort: Sealed + Send + Sync {
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
// BackgroundSupervisorPort — subagents, delegated workflow handles, run-finalization cleanup.
// ---------------------------------------------------------------------------

/// A started subagent handle (returned on the `Launched` arm of [`SpawnedSubagent`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedSubagent {
    /// The agent-facing background handle (`subagent_<n>`).
    pub subagent_session_id: SubagentSessionId,
}

/// Tool-owned launch facts for `run_subagent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentLaunch {
    /// Registered subagent name requested by the model.
    pub agent_name: String,
    /// User/model supplied subagent task prompt.
    pub prompt: String,
    /// Tool-owned launch guidance appended to the child run.
    pub guidance: String,
}

/// Typed launch rejection facts. Rendering stays in `eos-tools`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentLaunchRejection {
    /// The caller is already a subagent.
    Recursive,
    /// The requested agent name is not registered.
    NotRegistered {
        /// Requested agent name.
        agent_name: String,
    },
    /// The requested agent exists but is not subagent-typed.
    NotSubagent {
        /// Requested agent name.
        agent_name: String,
        /// Registered agent type string.
        agent_type: String,
    },
}

/// The outcome of [`BackgroundSupervisorPort::spawn`]: a tracked launch, or a
/// typed in-band validation rejection. Mirrors [`SubmissionAck`]: validation
/// failures (recursion / unknown / non-subagent) are model-facing `Ok(Rejected)`
/// outcomes, not `Err(ToolError)` framework faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnedSubagent {
    /// The subagent run was launched and is tracked.
    Launched(StartedSubagent),
    /// Validation rejected the dispatch.
    Rejected(SubagentLaunchRejection),
}

/// Background-session status facts returned by the engine for subagent control
/// tools. Rendering stays in `eos-tools`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentSessionStatus {
    /// The subagent is still running.
    Running,
    /// The subagent called its terminal tool.
    Completed,
    /// The subagent crashed or exited without terminal output.
    Failed,
    /// The subagent was cancelled.
    Cancelled,
    /// The subagent result was already delivered.
    Delivered,
}

/// A subagent progress snapshot for `check_subagent_progress`.
#[derive(Debug, Clone)]
pub struct SubagentProgressSnapshot {
    /// Agent-facing subagent session id.
    pub subagent_session_id: SubagentSessionId,
    /// Current tracked status.
    pub status: SubagentSessionStatus,
    /// Registered subagent name.
    pub agent_name: String,
    /// Terminal result, when available.
    pub result: Option<ToolResult>,
}

/// Result of looking up a tracked subagent session.
#[derive(Debug, Clone)]
pub enum SubagentProgress {
    /// The session exists.
    Found(SubagentProgressSnapshot),
    /// The session id is unknown to the owning run.
    Missing {
        /// Agent-facing subagent session id that was requested.
        subagent_session_id: SubagentSessionId,
    },
}

/// Result of a `cancel_subagent` request.
#[derive(Debug, Clone)]
pub enum CancelledSubagent {
    /// A running subagent was cancelled.
    Cancelled {
        /// Agent-facing subagent session id.
        subagent_session_id: SubagentSessionId,
        /// User/tool supplied cancellation reason.
        reason: String,
    },
    /// The session id is unknown or already terminal.
    MissingOrSettled {
        /// Agent-facing subagent session id that could not be cancelled.
        subagent_session_id: SubagentSessionId,
    },
}

/// Per-kind in-flight background-task count (Running records only) for one agent
/// run, serialized to JSON for the terminal-cleanup audit assertion. The owning
/// run is the supervisor handle's `owner_agent_run_id`, so there is no
/// `agent_run_id` field or filter (spec §8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RunningBackgroundTasks {
    /// `subagents + workflows + command_sessions`.
    pub total: usize,
    /// In-flight subagent runs for this agent run.
    pub subagents: usize,
    /// Outstanding delegated workflows for this agent run. The supervisor owns the
    /// background handle bookkeeping; [`WorkflowControlPort`] remains the source
    /// of truth for persisted workflow lifecycle.
    pub workflows: usize,
    /// In-flight, supervisor-tracked command sessions for this agent run
    /// (diagnostic; the authoritative live-session gate is the daemon RPC).
    pub command_sessions: usize,
}

/// The engine background supervisor surface used by subagent tools, workflow
/// delegation handle bookkeeping, and run-finalization cleanup. Implemented by
/// `eos-engine`.
#[async_trait]
pub trait BackgroundSupervisorPort: Sealed + Send + Sync {
    /// Validate, launch, and track a dispatchable subagent run. `ctx` is the
    /// caller's execution metadata: the implementor reads the caller identity
    /// from it (for recursion + the agent-run-scoped count) and clones it to build
    /// the child run's metadata. Validation failures (recursion / unknown /
    /// non-subagent) return `Ok(Rejected(_))`; `Err` is a framework fault only.
    async fn spawn(
        &self,
        ctx: &ExecutionMetadata,
        launch: SubagentLaunch,
    ) -> Result<SpawnedSubagent, ToolError>;

    /// Return a tracked subagent's status/result facts for the model-facing
    /// `check_subagent_progress` renderer.
    async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
        last_n_messages: u8,
    ) -> Result<SubagentProgress, ToolError>;

    /// Cancel a tracked subagent session and return the cancellation fact for
    /// the model-facing `cancel_subagent` renderer.
    async fn cancel(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> Result<CancelledSubagent, ToolError>;

    /// This agent run's in-flight background report (Running-only), without mutating
    /// state — the reject-mode read for `enter_isolated_workspace`. The handle
    /// scopes the count to its `owner_agent_run_id` (no filter argument, spec §10).
    async fn running_background_tasks(&self) -> RunningBackgroundTasks;

    /// Settle this agent run's in-flight subagent runs (`Cancelled` + abort) and
    /// return the post-cancel report. The terminal / exit prehook uses this
    /// lane-specific cleanup so a live or phantom subagent never wedges the
    /// terminal, while delegated workflows remain gated by persisted workflow
    /// state.
    async fn cancel_subagents(&self) -> RunningBackgroundTasks;

    /// Track a workflow that was just delegated by this agent run. The workflow
    /// control port owns persisted workflow state; the background supervisor owns
    /// the handle for in-flight accounting and run-finalization cancellation.
    async fn register_workflow(&self, workflow: &StartedWorkflowHandle);

    /// Mark a tracked workflow handle cancelled in the supervisor ledger.
    async fn cancel_workflow_record(
        &self,
        workflow_task_id: &WorkflowSessionId,
        reason: &str,
    ) -> bool;

    /// Tear down all background work owned by this agent run (spec §8.2): settle +
    /// abort subagents, cancel delegated workflows through the optional
    /// authoritative workflow-control port (a missing port still settles the
    /// in-memory record), and cancel all command sessions in one per-caller daemon
    /// RPC. The common run-finalization / cancellation finalizer.
    async fn teardown(
        &self,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> RunningBackgroundTasks;
}

// ---------------------------------------------------------------------------
// CommandSessionSupervisorPort — register / recover / mark / count background
// PTY command sessions.
// ---------------------------------------------------------------------------

/// The engine background supervisor's command-session surface, used by the
/// `exec_command`/`write_stdin` tools to track sandbox-bound background command
/// sessions and to recover a terminal result across the heartbeat race
/// (anchor §5, §8). Implemented by `eos-engine` on the same supervisor instance
/// as [`BackgroundSupervisorPort`].
///
/// The `result` payloads are the daemon completion's `result` map (status,
/// `exit_code`, `output.stdout`, …); they are opaque JSON to the supervisor and
/// rendered by the engine when delivered.
#[async_trait]
pub trait CommandSessionSupervisorPort: Sealed + Send + Sync {
    /// Register a freshly-started background command session as running. The
    /// `command_session_id` is the daemon-minted `cmd_<n>` correlation key; the
    /// owning run is the handle's `owner_agent_run_id` (no `agent_run_id` arg).
    async fn register(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
        command: &str,
    );

    /// The stored terminal result for a session whose live daemon session is
    /// already gone (the recover race), or `None` when it is still running or
    /// untracked.
    async fn command_session_result(&self, command_session_id: &CommandSessionId) -> Option<Value>;

    /// Mark a session reported (delivered) with the terminal `result` a control
    /// tool observed inline, so the heartbeat does not re-deliver it.
    async fn mark_command_session_reported(
        &self,
        command_session_id: &CommandSessionId,
        result: Value,
    );

    /// Whether a session's completion was already delivered to the model (via the
    /// heartbeat). A late command-session control tool uses this to return a
    /// terse already-reported note instead of re-dumping the completion.
    async fn command_session_already_reported(&self, command_session_id: &CommandSessionId)
        -> bool;
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

// ---------------------------------------------------------------------------
// CancelableResource / CancelPort — recursive agent-core cancellation (spec §7).
// ---------------------------------------------------------------------------

/// A non-leaf effect a tool creates that must be torn down on cancellation.
///
/// Implemented by the engine's foreground/background resource handles
/// (workflow handle, subagent handle, inline advisor run). Command sessions are
/// **not** per-resource `CancelableResource`s — they are daemon-owned and torn
/// down by one per-caller daemon RPC, not a per-session teardown.
#[async_trait]
pub trait CancelableResource: Send + Sync {
    /// Tear down the spawned effect. `reason` is propagated for audit.
    async fn teardown(&self, reason: &str) -> Result<(), ToolError>;
}

/// The two recursive agent-core cancellation primitives.
///
/// The trait is owned here to avoid an `eos-engine` <-> `eos-workflow` crate
/// cycle; the implementation lives in `eos-engine` and the recursive workflow
/// decomposition (`cancel_workflow -> cancel_iteration -> cancel_attempt`) calls
/// back through this port. Both methods are awaited end-to-end and idempotent.
#[async_trait]
pub trait CancelPort: Send + Sync {
    /// Cancel a persisted task: flip `{Pending,Running} -> Cancelled` and, if a
    /// live run owns the task, recurse into [`CancelPort::cancel_agent_run`].
    async fn cancel_task(&self, task_id: &TaskId, reason: &str) -> Result<(), ToolError>;

    /// Cancel a live agent run: request cooperative cancellation, tear down its
    /// foreground and background resources, and finalize its records.
    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), ToolError>;
}

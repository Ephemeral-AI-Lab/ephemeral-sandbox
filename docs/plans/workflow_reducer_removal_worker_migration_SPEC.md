# Workflow Reducer Removal And Worker Migration - SPEC

Status: Proposed (rev 2)
Date: 2026-06-09
Owner: eos-workflow / eos-types / eos-tool / eos-agent-run / eos-db / agent profiles

Scope:
- `agent-core/crates/eos-types`
- `agent-core/crates/eos-workflow`
- `agent-core/crates/eos-tool`
- `agent-core/crates/eos-agent-run`
- `agent-core/crates/eos-db`
- `.eos-agents/profile`
- `.eos-agents/tools`
- `.eos-agents/skills`

## 0. Revision 2 — adopted simplifications

This revision folds in the review findings. The two structural changes that drive
everything else:

1. **The planner keeps its TaskStore row.** A task row does two independent jobs:
   (a) membership in the worker DAG and (b) being the *record anchor* for an agent
   run's transcript (`messages.jsonl`/`events.jsonl` + nested
   `workflows`/`subagents`/`advisors`). The planner needs (b), not (a). Every
   `AgentType::Agent` run (root, planner, worker) is task-anchored; making the
   planner task-less would make it the lone homeless agent run and break
   `format_record_dir` / `finish_task_run`. **Decision: the planner is task-owned
   for recording but is never a worker-DAG member.** Drop only the redundant
   `PlannerId` (one planner per attempt → derive `planner_task_id(attempt_id)`).

2. **Typed per-family outcomes aligned 1:1 with role — `task outcome === agent
   outcome === terminal tool outcome`.** Every terminal tool records a single typed,
   serde-tagged outcome into the run's existing `terminal_payload` column. There are
   two families, split by `AgentType`, and each aligns its role and outcome 1:1:
   - **Workflow-task family** (`AgentType::Agent`): `TaskRole { Root, Planner, Worker }`
     ⟷ `TaskOutcome { Root, Planner, Worker }`.
   - **Parented family** (`AgentType::{Subagent, Advisor}`):
     `ParentedAgentRunKind { Subagent, Advisor }` ⟷ `ParentedOutcome { Subagent, Advisor }`.

   This deletes the separate `PlanOutcome`, `WorkItemOutcome`, `AttemptOutcome`, and
   `ExecutionTaskOutcome` DTOs. The plan outcome is just `TaskOutcome::Planner` — a
   JSON object exactly like the root and worker outcomes. Pass/fail is **never**
   stored on an outcome; it is read from the owning `TaskStatus`. Do not merge the
   two families into one enum — that breaks the `TaskRole ↔ TaskOutcome` 1:1.

Downstream simplifications adopted: delete `MaterializedPlan`/`PlanDisposition`;
use the typed `DeferredGoal` newtype (not `String`) on internal contracts;
`SubmissionStatus` lives only on the thin model-facing inputs and maps onto
`TaskStatus`; dissolve `eos-workflow/src/ids.rs` and `state/projections.rs`; rename
`TaskStatus::is_terminal_generator` → `is_terminal`; drop `submission_kind` /
`disposition` from terminal metadata; collapse the context layer to three files.

## 1. Intent

This is an aggressive cleanup migration. The target removes reducer as a workflow
role, converts generator terminology to worker terminology, removes the binary
generator/reducer execution model, and unifies every terminal payload under one
`TaskOutcome` type.

The workflow model becomes:

```text
Workflow
  -> Iteration[]
      -> Attempt[]
          -> planner task   (TaskOutcome::Plan)
          -> worker task[]  (TaskOutcome::Worker)
```

Every node is a TaskStore row carrying exactly one `TaskOutcome` on its
`terminal_payload`. The question "did it succeed?" is answered by the existing
lifecycle state, never by a field on `TaskOutcome`:

```text
TaskStatus
AttemptStatus
IterationStatus
WorkflowStatus
```

`TaskOutcome` carries only the durable terminal payload that is not already
represented by lifecycle state. Attempt/iteration/workflow outcomes are **read-side
projections** over task `TaskOutcome`s + their `TaskStatus`; they are not stored
aggregation DTOs.

Root, advisor, and subagent terminal payloads are also `TaskOutcome` variants on
their own task/parented rows; they are not aggregated into workflow outcome DTOs.

## 2. Decisions

| Area | Decision |
| --- | --- |
| Reducer role | Delete. No reducer rows, tasks, launches, outcomes, context recipes, terminal tools, or reducer profile/skill files. |
| Generator role | Replace with worker. Public workflow contracts use `Worker` / `WorkItem`; no `Generator` public contract remains. |
| Planner identity | **Planner keeps its TaskStore row** (recording anchor) but is never a worker-DAG member. `PlannerId` is removed; the planner task id is derived `planner_task_id(attempt_id)`. The model never sends a planner task id. |
| Worker identity | Worker rows are the worker-DAG TaskStore rows. Worker task ids are derived from `(AttemptId, WorkItemId)`. |
| Plan payload | `plan_spec` plus `work_items`; no `task_specs`, no `reducers`, no `disposition`. |
| Work item payload | Each work item carries its own `work_spec`; materialized into the worker task's instruction. No separate `task_specs` map. |
| Outcome model | One `TaskOutcome` enum (`task outcome === agent outcome === terminal tool outcome`), stored as `Task.terminal_payload`. Delete `PlanOutcome`, `WorkItemOutcome`, `AttemptOutcome`, `ExecutionTaskOutcome`, and `Task.outcomes`. |
| Success/failure | Use `TaskStatus`, `AttemptStatus`, `IterationStatus`, `WorkflowStatus`. Do not add `status: bool`, `is_success`, or any status field to `TaskOutcome` or to aggregation projections. |
| Outcome text | One `outcome: String` field on the execution variants (`Root`, `Worker`, `Advisor`, `Subagent`). The `Plan` variant has no free-text `outcome`; its body is `plan_spec` + `work_items`. |
| Plan storage | The authored plan lives only in the planner task's `TaskOutcome::Plan` (single source of truth). It is materialized into worker task rows; the attempt row stores no plan copy and no task-id lists. |
| Context data | No public context projection DTOs. Filtering is local to `eos-workflow` context render functions. |
| Record paths | Planner records stay **task-owned** (planner has a task row). Worker records are task-owned. `format_record_dir` / `finish_task_run` are unchanged. |

Do not introduce these names:

- `PlanWorkItem`
- `disposition`
- `submission_kind`
- planner `work_item_id`
- workflow `task_specs`
- workflow reducer compatibility aliases
- `has_structured_outcome`
- `is_successful`
- `user_result`
- `work_result`
- `review_summary`
- `answer`
- `work_instruction`
- `direct_needs`
- `direct_need_outcomes`
- `assigned_work_item`
- `agent_profile_name`
- `worker_task_by_work_item_id`
- `ContextOutcomeSlice`
- `ContextOutcomeView`
- `AttemptOutcomeForContext`
- `IterationOutcomeForContext`
- `WorkflowNodeId`
- `MaterializedPlan`
- `PlanDisposition`
- `ExecutionRole`
- `ExecutionTaskOutcome`
- `PlanOutcome` / `WorkItemOutcome` / `AttemptOutcome` (folded into `TaskOutcome`)

Naming rules:

| Surface | Rule |
| --- | --- |
| Workflow files | Name files after the runtime ownership they contain: `attempt_run`, `planner_run`, `work_items`, `work_items_run`, `workflow_run`, `iteration_run`. |
| Work item execution | Use `work_items_run` for worker wave execution and settlement. Do not use `work_dag`, `plan_dag`, `node`, `stage`, or `orchestrator` names for this owner. |
| Plan shape | Use `WorkItemSpec` for planner-authored work items. Do not introduce `PlanWorkItem`. |
| Outcome type | One `TaskOutcome` enum, one variant per terminal tool. Persisted as the task's `terminal_payload`. Do not reintroduce per-role outcome DTOs. |
| Terminal text | Use one `outcome` field for natural-language terminal payloads. Do not split it into `answer`, `summary`, `user_result`, `work_result`, or `review_summary`. |
| Success/failure | Use lifecycle enums. `TaskOutcome` carries no status. Model-facing inputs carry `SubmissionStatus` (maps onto `TaskStatus`). |
| Dependencies | Use `needs` for direct work item dependencies. Do not add `direct_needs` or `direct_need_outcomes`. |
| Worker assignment | Use `agent_name: AgentName` on `WorkItemSpec`. Do not use `agent_profile_name` or `assigned_work_item`. |
| Deferred goal | Use the typed `DeferredGoal` newtype on internal contracts. Raw `String` is allowed only on the model-facing `*Input` wire DTOs. |
| Task mapping | Use deterministic `worker_task_id(attempt_id, work_item_id)` and `planner_task_id(attempt_id)`. Do not persist a worker-task id list. |

## 3. Resulting File And Folder Structure

Target structure under `agent-core`:

```text
agent-core/crates/eos-types/src/
  contracts/
    record.rs                    # TaskAgentRunKind (Workflow unchanged); WorkflowTaskRole = {Planner, Worker}
    workflow.rs                  # WorkflowApi + 2-method attempt submission API
  state/
    request_task/
      task.rs                    # TaskRole::{Root, Planner, Worker}; TaskStatus::is_terminal
    tools/
      submissions.rs             # PlanOutcomeSubmission, WorkerOutcomeSubmission (internal)
    workflow/
      workflow.rs                # Workflow lifecycle DTOs
      iteration.rs               # Iteration lifecycle DTOs
      attempt.rs                 # Attempt lifecycle DTOs (no MaterializedPlan)
      work_item.rs               # WorkItemId, WorkItemSpec, worker_task_id, planner_task_id
      outcome.rs                 # TaskOutcome enum (Root, Worker, Plan, Advisor, Subagent)

agent-core/crates/eos-workflow/src/
  attempt/
    attempt_run.rs               # thin attempt coordinator (start/close/asserts)
    active_attempt_runs.rs       # in-process planner abort / active attempt handles
                                 #   + OpenIterationCoordinatorRegistry home
    planner_run.rs               # planner launch + settle (task-owned; signal = planner TaskStatus)
    work_items.rs                # plan validation, worker materialization, readiness helpers
    work_items_run.rs            # worker waves, worker settlement, missing-outcome synthesis,
                                 #   worker-outcome collection
    launch.rs                    # AgentLaunch (struct+kind), AgentLaunchFactory, AgentRunner,
                                 #   AttemptResources
  context/
    planner_context.rs           # all planner cases (one "exactly one of" match)
    worker_context.rs            # plan_spec + needs + work_item (+ dependency rendering)
    render.rs                    # recipe dispatch + xml/section render currency
    composer.rs                  # AgentEntryComposer (skill/terminal-block plumbing)
    scope.rs                     # ContextScope::{Planner, Worker}
  workflow_run.rs                # WorkflowApi start/check/cancel + create/close_workflow
  iteration_run.rs               # coordinator + retry + continuation + handle_iteration_closed
  attempt_submission.rs          # submit_plan_outcome + submit_worker_outcome adapter
  config.rs                      # WorkflowLifecycleConfig (relocated from the deleted ids.rs)
  # DELETED: ids.rs, state/projections.rs, attempt/{orchestrator,run_stage,plan_dag,
  #          orchestrator_registry}.rs

agent-core/crates/eos-tool/src/
  model.rs                       # ToolName submit_* rename set (6 -> 5 terminals)
  registry.rs
  tools/
    terminal.rs                  # TerminalTool::{RootTask, Plan, Worker, Advisor, Subagent}
    submission/
      mod.rs
      support.rs                 # SubmissionStatus (wire), OutcomeInput, shared helpers
      submit_root_task_outcome.rs
      submit_plan_outcome.rs
      submit_worker_outcome.rs
      submit_advisor_outcome.rs
      submit_subagent_outcome.rs

agent-core/crates/eos-db/         # column narrowing; edit 0001_initial.sql in place
                                  #   (contingent on no deployed DB needing migration)
  # attempts: KEEP planner_task_id; DROP generator_task_ids, reducer_task_ids,
  #           the attempt `outcomes` cache, and the attempt `deferred_goal` cache.
  #           Plan lives in the planner task's TaskOutcome::Plan; iteration deferral
  #           lives in Iteration.deferred_goal_for_next_iteration.
  # task_runs CHECK: role IN ('planner','worker')
  # rows.rs: drop MaterializedPlan reconstruction + 'generator'/'reducer' parsing;
  #          terminal_payload deserializes as TaskOutcome

.eos-agents/profile/
  main/
    root.md
    planner.md
    executor.md                  # worker-capable profile; context_recipe = worker
  helper/
    advisor.md
  subagent/
    subagent.md

.eos-agents/tools/
  submit_root_task_outcome.md
  submit_plan_outcome.md
  submit_worker_outcome.md
  submit_advisor_outcome.md
  submit_subagent_outcome.md
```

Remove the old mechanism-oriented file names from the target workflow path:

| Delete / replace | Reason |
| --- | --- |
| `attempt/orchestrator.rs` | Too broad; split into attempt, planner, and worker run ownership. |
| `attempt/orchestrator_registry.rs` | Rename to `active_attempt_runs.rs`. |
| `attempt/plan_dag.rs` | Names a data structure, not workflow ownership; folds into `work_items.rs`. |
| `attempt/run_stage.rs` | Names a stage, not the worker-run owner; folds into `work_items_run.rs`. |
| `context/engine.rs` | Too generic; split into `render.rs` + role renderers. |
| `state/projections.rs` | Dissolves: its only real logic (the reducer-gate filter) is deleted; the residual worker-outcome walk moves to `work_items_run.rs` settlement, rendering to `context/render.rs`. |
| `ids.rs` | Dissolves: id derivation moves to eos-types `work_item.rs`; `WorkflowLifecycleConfig` moves to `config.rs`. |
| `tools/submission.rs` | Too large; one per-tool file named after each wire tool. |

## 4. Files And Concepts To Delete

Delete files:

```text
.eos-agents/profile/main/reducer.md
.eos-agents/skills/reducer/
.eos-agents/tools/submit_generator_outcome.md
.eos-agents/tools/submit_reducer_outcome.md
.eos-agents/tools/submit_planner_outcome.md
.eos-agents/tools/submit_root_outcome.md
.eos-agents/tools/submit_advisor_feedback.md
.eos-agents/tools/submit_subagent_result.md
```

Replace with:

```text
.eos-agents/tools/submit_worker_outcome.md
.eos-agents/tools/submit_plan_outcome.md
.eos-agents/tools/submit_root_task_outcome.md
.eos-agents/tools/submit_advisor_outcome.md
.eos-agents/tools/submit_subagent_outcome.md
```

Delete or rewrite code concepts:

| Current concept | Target |
| --- | --- |
| `TaskRole::Planner` | **keep** (planner has a task row; never a worker-DAG member) |
| `TaskRole::Generator` | `TaskRole::Worker` |
| `TaskRole::Reducer` | delete |
| `WorkflowTaskRole::{Planner, Generator, Reducer}` | `WorkflowTaskRole::{Planner, Worker}` (record-path label) |
| `WorkflowNodeId` | delete (spawn target carries `WorkflowTaskRole` + worker `work_item_id`) |
| `ExecutionRole` | delete |
| `ExecutionTaskOutcome` | delete (use `TaskOutcome` + `TaskStatus`) |
| `Task.outcomes: Vec<ExecutionTaskOutcome>` | delete (use `Task.terminal_payload: TaskOutcome`) |
| `MaterializedPlan` | delete (plan lives in `TaskOutcome::Plan`; workers materialized as task rows) |
| `PlanDisposition` | delete (use `Option<DeferredGoal>`) |
| `PlanTask` | `WorkItemSpec` |
| `PlanReducer` | delete |
| `GeneratorId` | `WorkItemId` |
| `ReducerId` | delete |
| `PlannerId` | delete (derive `planner_task_id(attempt_id)`) |
| `PlannerPlan.{planner_task_id, disposition, tasks, task_specs, reducers}` | `TaskOutcome::Plan { plan_spec, work_items, deferred_goal_for_next_iteration }` |
| `GeneratorSubmission` / `ReducerSubmission` | `WorkerOutcomeSubmission` |
| `PlannerSubmission` / `PlannerFailureSubmission` / `PlannerFailReason` | `PlanOutcomeSubmission`; planner failure is an attempt lifecycle transition |
| `PlanOutcome` / `WorkItemOutcome` / `AttemptOutcome` DTOs | `TaskOutcome` variants + read-side projection |

## 5. IDs

| ID | Action | Reason |
| --- | --- | --- |
| `PlannerId` | remove | One planner per attempt; derive `planner_task_id(attempt_id)`. |
| `GeneratorId` | replace with `WorkItemId` | The planner authors work items, not generators. |
| `ReducerId` | remove | Reducer role is deleted. |
| planner `TaskId` | **keep** | The planner keeps its TaskStore row as the record anchor; the id is derived, not authored. |
| reducer `TaskId` | remove | Reducer rows no longer exist. |
| worker `TaskId` | keep | TaskStore owns persisted worker rows. |
| `WorkItemId` | add | Workflow-local id authored by the planner and used in `needs`. |
| `AttemptId` / `IterationId` / `WorkflowId` | keep | Lifecycle aggregation keys. |

Deterministic task ids (both owned by eos-types `state/workflow/work_item.rs`):

```rust
pub fn planner_task_id(attempt_id: &AttemptId) -> TaskId;                       // one per attempt
pub fn worker_task_id(attempt_id: &AttemptId, work_item_id: &WorkItemId) -> TaskId;
```

Do not persist a worker-task id list. Enumerate an attempt's worker tasks either by
querying tasks by `(attempt_id, role = Worker)` or by mapping
`worker_task_id(attempt_id, w.id)` over the planner's recorded `work_items`.

## 6. Target Contracts

```rust
pub struct WorkItemSpec {
    /// Planner-authored workflow-local id.
    pub id: WorkItemId,
    /// Selected worker-capable agent profile name.
    pub agent_name: AgentName,
    /// Executable work instruction (becomes the worker task's instruction).
    pub work_spec: String,
    /// Direct work item dependencies. Context edges, not scheduling shortcuts.
    pub needs: Vec<WorkItemId>,
}

/// task outcome === agent outcome === terminal tool outcome.
/// Stored verbatim as Task.terminal_payload / ParentedRun.terminal_payload.
/// Pass/fail is NOT here — it is the owning TaskStatus. One variant per terminal.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskOutcome {
    /// Root request result (user-facing).
    Root { outcome: String },
    /// One worker's deliverable or blocker.
    Worker { outcome: String },
    /// The planner's authored plan (single source of truth for the attempt plan).
    Plan {
        plan_spec: String,
        work_items: Vec<WorkItemSpec>,
        /// Concrete current-iteration goal items carried to the next iteration.
        deferred_goal_for_next_iteration: Option<DeferredGoal>,
    },
    /// Advisor review verdict + rationale.
    Advisor { verdict: AdvisorVerdict, outcome: String },
    /// Subagent findings.
    Subagent { outcome: String },
}

/// Attempt lifecycle state. The plan is NOT copied here — it lives in the planner
/// task's TaskOutcome::Plan. No MaterializedPlan.
pub enum AttemptState {
    /// Planner task assigned; the None -> Some transition guards double-start.
    Planning { planner_task_id: Option<TaskId> },
    /// Worker DAG materialized and running.
    Running { planner_task_id: TaskId },
    /// Terminal.
    Closed { closure: AttemptClosure, planner_task_id: Option<TaskId> },
}
```

Attempt/iteration/workflow outcomes are **read-side projections**, not stored DTOs:

```rust
// derived on read; pass/fail from TaskStatus, never from a stored boolean
fn attempt_outcome(attempt, tasks) -> {
    plan:    planner_task.terminal_payload,                 // TaskOutcome::Plan
    workers: worker_tasks.map(|t| (t.status, t.terminal_payload)),  // status + TaskOutcome::Worker
}
```

`IterationStatus` / `WorkflowStatus` remain the lifecycle authority. Do not add
`IterationOutcome.status`, `WorkflowOutcome.status`, or any `is_success` boolean.

## 7. Model-Facing Tool Contracts

Each terminal keeps a small model-facing input. The terminal maps the input onto
the owning `TaskStatus` and records exactly one `TaskOutcome` variant on the task's
`terminal_payload`. The stored `TaskOutcome` never re-stores status.

### `submit_root_task_outcome`

```rust
pub struct SubmitRootTaskOutcomeInput { pub status: SubmissionStatus, pub outcome: String }
```

Maps `SubmissionStatus` onto the root task's `TaskStatus` (and `RequestStatus`) and
records `TaskOutcome::Root { outcome }`.

### `submit_plan_outcome`

```rust
pub struct SubmitPlanOutcomeInput {
    pub plan_spec: String,
    #[serde(default)]
    pub deferred_goal_for_next_iteration: Option<String>,   // wire String; -> DeferredGoal on ingest
    pub work_items: Vec<WorkItemSpecInput>,
}

pub struct WorkItemSpecInput {
    pub id: String,
    pub agent_name: String,
    pub work_spec: String,
    #[serde(default)]
    pub needs: Vec<String>,
}
```

Internal submission:

```rust
pub struct PlanOutcomeSubmission {
    pub attempt_id: AttemptId,
    pub plan_spec: String,
    pub work_items: Vec<WorkItemSpec>,
    pub deferred_goal_for_next_iteration: Option<DeferredGoal>,
}
```

Rules:

- A model-submitted plan records `TaskOutcome::Plan` on the planner task and sets
  the planner task `TaskStatus::Done`.
- A planner that fails to return creates no `TaskOutcome::Plan`; the runtime marks
  the planner task `Failed` (synthesized) and the attempt closes failed.
- `SubmitPlanOutcomeInput` has no `status` (a returned plan is success by
  construction) and the model never sends a planner task id.
- Terminal metadata may include `attempt_id` and
  `has_deferred_goal_for_next_iteration`; it must not include `disposition` or
  `submission_kind`.

Model JSON:

```json
{
  "plan_spec": "Implement and verify the migration in focused worker items.",
  "deferred_goal_for_next_iteration": null,
  "work_items": [
    {
      "id": "w1",
      "agent_name": "executor",
      "work_spec": "Replace generator/reducer workflow DTOs with worker DTOs.",
      "needs": []
    }
  ]
}
```

### `submit_worker_outcome`

```rust
pub struct SubmitWorkerOutcomeInput { pub status: SubmissionStatus, pub outcome: String }
```

Internal submission:

```rust
pub struct WorkerOutcomeSubmission {
    pub attempt_id: AttemptId,
    pub task_id: TaskId,
    pub work_item_id: WorkItemId,
    pub status: SubmissionStatus,   // maps onto the worker task's TaskStatus
    pub outcome: String,
}
```

Rules:

- The terminal maps `SubmissionStatus` onto the worker task's `TaskStatus` and
  records `TaskOutcome::Worker { outcome }`.
- A worker that fails to return is synthesized by workflow runtime as a failed
  worker task whose `TaskOutcome::Worker.outcome` explains the missing terminal.
- The worker is keyed by both persisted `task_id` and planner-authored
  `work_item_id`.
- Terminal metadata may include `attempt_id`, `task_id`, `work_item_id`; it must not
  include `submission_kind`.

Model JSON:

```json
{ "status": "success", "outcome": "Implemented the assigned change and verified it with cargo check." }
```

### `submit_advisor_outcome`

```rust
pub struct SubmitAdvisorOutcomeInput { pub verdict: AdvisorVerdict, pub outcome: String }
```

Records `TaskOutcome::Advisor { verdict, outcome }`. Renames `submit_advisor_feedback`.
`verdict` is the advisor's review result, not a lifecycle status.

### `submit_subagent_outcome`

```rust
pub struct SubmitSubagentOutcomeInput { pub outcome: String }
```

Records `TaskOutcome::Subagent { outcome }`. Renames `submit_subagent_result`. Do
not keep a half-typed `findings`/`references` shape.

## 8. Workflow Submission API

Replace the three-method attempt submission API with two methods:

```rust
#[async_trait]
pub trait WorkflowAttemptSubmissionApi: Send + Sync {
    async fn submit_plan_outcome(&self, submission: PlanOutcomeSubmission)
        -> Result<SubmissionAck, CoreError>;

    async fn submit_worker_outcome(&self, submission: WorkerOutcomeSubmission)
        -> Result<SubmissionAck, CoreError>;
}
```

Delete `apply_plan` / `submit_generator` / `apply_reducer`.

## 9. Tool Name Diff

| Current | Target | Action |
| --- | --- | --- |
| `submit_root_outcome` | `submit_root_task_outcome` | rename |
| `submit_planner_outcome` | `submit_plan_outcome` | rename; no planner task id from the model |
| `submit_generator_outcome` | `submit_worker_outcome` | replace |
| `submit_reducer_outcome` | none | delete |
| `submit_advisor_feedback` | `submit_advisor_outcome` | rename |
| `submit_subagent_result` | `submit_subagent_outcome` | rename |

Terminal tool enum target (1:1 with the `TaskOutcome` variants):

```rust
pub enum TerminalTool { RootTask, Plan, Worker, Advisor, Subagent }
```

`ToolName::ALL` shrinks by one terminal after deleting reducer:

```text
SubmitRootTaskOutcome
SubmitPlanOutcome
SubmitWorkerOutcome
SubmitAdvisorOutcome
SubmitSubagentOutcome
```

## 10. Work Item Plan Contract

Rules:

- Work item ids are unique within the attempt.
- `agent_name` is required and must resolve to a worker-capable (`AgentType::Agent`)
  profile.
- `work_spec` is required and nonblank.
- `needs` may reference only work item ids in the same plan.
- `needs` are direct context inputs, not scheduling shortcuts.
- A worker receives only the outcomes of the work items in its own `needs`;
  transitive ancestors are not included unless listed directly.
- At least one work item is required.
- There is no special sink node. A work item with no downstream dependents is a
  valid leaf and contributes directly to the attempt result.

Plan validation (`work_items.rs`) is the residual of the old `validate_plan_shape`:
unique work item ids, `needs` reference known ids, acyclic, at least one item.
**Deleted** reducer rules: the `>=1 reducer` requirement, reducer-needs rules, the
fixed-reducer-profile check, and the "dangling generator with no downstream"
rejection (leaves are now valid). `assert_acyclic` survives unchanged modulo the
`GeneratorId` -> `WorkItemId` rename.

This is the key simplification: every leaf worker is already a reducer for its own
branch. Attempt success is derived from worker task statuses, not from a separate
reducer row.

## 11. Outcome Aggregation Rules

Attempt:

- The planner records `TaskOutcome::Plan` on its task and is marked `Done`; a
  planner that never submits is marked `Failed` and the attempt closes failed.
- Worker tasks are materialized from `work_items` (instruction = `work_spec`, needs
  = `needs` mapped through `worker_task_id`). Each launched worker that returns
  records `TaskOutcome::Worker` and updates its `TaskStatus`.
- A worker that fails to return is synthesized as a failed worker task with a
  `TaskOutcome::Worker` explaining the missing terminal.
- The attempt passes only when every required worker task is `Done`.
- The attempt fails when any required worker task is `Failed`, `Blocked`, or
  `Cancelled`, or when worker readiness reaches a failed quiescent state. (The
  existing `dag_resolution` "all Done" / quiescence logic is unchanged.)

Iteration:

- Iteration status is derived from the accepted terminal attempt.
- The deferred goal flows to `Iteration.deferred_goal_for_next_iteration`.
- Keep attempt ids available so retry history is inspectable; outcomes are read
  from each attempt's task `TaskOutcome`s, not a stored attempt cache.

Workflow:

- Workflow status is derived from the latest terminal iteration and whether a
  deferred goal requires another iteration.
- Workflow rendering defaults to the latest iteration only.

## 12. Context Recipe Design

Context renders role-specific text directly from workflow state + task
`TaskOutcome`s. No public `ContextOutcomeSlice`/`ContextOutcomeView` contract.

Target recipes:

```rust
pub enum ContextRole { Planner, Worker }

pub enum ContextScope {
    Planner { workflow_id: WorkflowId, iteration_id: IterationId, attempt_id: AttemptId },
    Worker {
        workflow_id: WorkflowId,
        iteration_id: IterationId,
        attempt_id: AttemptId,
        task_id: TaskId,
        work_item_id: WorkItemId,
    },
}
```

The recipe router validates `recipe_id = "planner"` only with `ContextRole::Planner`
and `recipe_id = "worker"` only with `ContextRole::Worker`.

Context files (three, not five): `planner_context.rs`, `worker_context.rs`,
`render.rs`. The planner's first-attempt / retry / continuation cases are **one
match** in `planner_context.rs` selecting exactly one evidence group — not three
files.

### Planner Recipe

Planner context (built from workflow/iteration/attempt lifecycle position) always
includes `<workflow_goal>` and `<current_iteration_goal>`, plus **exactly one** of:

- `<previous_attempts>`: retry evidence from failed attempts in the current
  iteration (read from those attempts' worker task `TaskOutcome::Worker` +
  `TaskStatus`).
- `<latest_iteration>`: continuation evidence from the latest successful previous
  iteration.

Planner context must not include planner task rows as evidence, full historical
summaries, all old iterations by default, or `disposition`.

Planner directive:

```text
Create one work item plan for the current iteration goal. Submit exactly one
plan outcome. Use deferred_goal_for_next_iteration only for concrete current
iteration goal items intentionally carried into the next iteration.
```

### Worker Recipe

Worker context includes:

- `<plan_spec>`: the planner's plan-level explanation (from `TaskOutcome::Plan`).
- `<work_item>`: this worker's `work_item_id`, `task_id`, and `work_spec`.
- `<needs>`: direct dependency outcomes only — each need's worker task
  `TaskOutcome::Worker.outcome`, resolved by mapping the `WorkItemId` edge through
  `worker_task_id`.

Worker context must not include the full plan, transitive dependency outcomes,
unrelated siblings, reducer guidance, or workflow lifecycle decisions.

Worker directive:

```text
Complete <work_item> using <plan_spec> and direct <needs>. Submit exactly one
worker outcome.
```

Worker render shape:

```xml
<context role="worker">
  <plan_spec>The planner-level explanation of how this attempt is structured.</plan_spec>
  <needs>
    <work_item id="w1" task_id="task_...">
      <outcome>Direct dependency outcome.</outcome>
    </work_item>
  </needs>
  <work_item id="w2" task_id="task_...">
    <agent_name>executor</agent_name>
    <work_spec>The exact instruction for this worker only.</work_spec>
  </work_item>
</context>
```

Direct-needs example `w1 -> w2 -> w3`: if `w3.needs = ["w2"]`, worker `w3` sees `w2`
only, not `w1`, unless the planner sets `"needs": ["w1", "w2"]`. Filtering lives in
`eos-workflow/src/context/*`, not in stores; stores return complete records.

## 13. Implementation Migration Phases

### Phase 1 - Types, DB, And Records

- Add `WorkItemId`, `WorkItemSpec`, `worker_task_id`, `planner_task_id`, the
  `TaskOutcome` enum, and the submission structs.
- Delete `MaterializedPlan`, `PlanDisposition`, `ExecutionRole`,
  `ExecutionTaskOutcome`, `PlannerId`, `GeneratorId`, `ReducerId`, `WorkflowNodeId`,
  and `Task.outcomes`; rename `is_terminal_generator` -> `is_terminal`.
- `TaskRole` -> `{Root, Planner, Worker}`; `WorkflowTaskRole` -> `{Planner, Worker}`.
- `eos-db`: keep `planner_task_id`; drop `generator_task_ids`/`reducer_task_ids`,
  the attempt `outcomes` cache, and the attempt `deferred_goal` cache; `task_runs`
  CHECK -> `role IN ('planner','worker')`; `terminal_payload` deserializes as
  `TaskOutcome`. Edit `0001_initial.sql` in place (verify no deployed DB).
- `eos-agent-run`: planner and worker records both stay task-owned (no change to
  the record/finish path beyond the role-label set).

Verification:

```text
cd agent-core && cargo check -p eos-types --all-targets
cd agent-core && cargo check -p eos-db --all-targets
cd agent-core && cargo check -p eos-agent-run --all-targets
```

### Phase 2 - Tools

- Rename terminal tool names and model-facing docs; delete the reducer terminal;
  replace generator with worker.
- Split `tools/submission.rs` into per-tool files under `submission/`; shared
  `SubmissionStatus` (wire), `OutcomeInput`, and helpers go in `support.rs`.
- Each terminal records the matching `TaskOutcome` variant; drop `submission_kind`
  from all terminal metadata.

Verification:

```text
cd agent-core && cargo check -p eos-tool --all-targets
```

### Phase 3 - Workflow Runtime

- Keep planner TaskStore row creation; remove `PlannerId` (derive
  `planner_task_id(attempt_id)`); planner is never a worker-DAG member.
- Replace `GeneratorLaunch`/`ReducerLaunch` with the worker arm of the `AgentLaunch`
  struct+kind (`task_id` shared; `kind = Planner | Worker { work_item_id, needs }`).
- Materialize one worker task row per work item; readiness over deterministic
  `worker_task_id`.
- Rewrite worker run settlement and missing-terminal synthesis to record
  `TaskOutcome::Worker`.
- Dissolve `ids.rs` (-> eos-types) and `state/projections.rs` (-> settlement +
  render).

Verification:

```text
cd agent-core && cargo test -p eos-workflow attempt -- --nocapture
```

### Phase 4 - Outcome Aggregation

- Aggregate attempts/iterations as read-side projections over task `TaskOutcome`s +
  `TaskStatus`; remove any stored attempt outcome cache.
- Close attempts from worker task statuses; close workflows by reading worker leaves
  directly.

Verification:

```text
cd agent-core && cargo test -p eos-workflow iteration -- --nocapture
cd agent-core && cargo test -p eos-workflow service -- --nocapture
```

### Phase 5 - Context Recipes

- `ContextScope`/`ContextRole` -> `{Planner, Worker}`; collapse context to
  `planner_context.rs` + `worker_context.rs` + `render.rs`.
- Render planner context from iteration outcomes + current failed attempts (exactly
  one evidence group); render worker context from `plan_spec`, the work item, and
  direct needs outcomes (read from sibling `TaskOutcome::Worker`).
- Update `.eos-agents/profile/main/planner.md` and `executor.md` (recipe `worker`,
  terminal `submit_worker_outcome`).

Verification:

```text
cd agent-core && cargo test -p eos-workflow context -- --nocapture
```

### Phase 6 - Cleanup Gate

- Delete reducer files, generated references, stale snapshots, and stale docs.
- Remove `Generator*`, `Reducer*`, `MaterializedPlan`, `PlanDisposition`,
  `ExecutionRole`, and reducer-specific profile/tool docs.

Verification:

```text
cd agent-core && cargo check --workspace --all-targets
cd agent-core && cargo test --workspace
rg "Generator|generator|Reducer|reducer|submit_generator_outcome|submit_reducer_outcome|disposition|submission_kind|MaterializedPlan|ExecutionRole|ExecutionTaskOutcome|WorkflowNodeId|is_terminal_generator" agent-core .eos-agents docs
```

Remaining matches must be historical migration docs or explicit compatibility notes
scheduled for deletion.

## 14. Acceptance Criteria

- No reducer TaskStore row can be created.
- Exactly one planner TaskStore row per attempt; the planner is never a worker-DAG
  member and the model never sends a planner task id.
- No model-facing tool named `submit_generator_outcome`, `submit_reducer_outcome`,
  or `submit_planner_outcome` is registered.
- One `TaskOutcome` enum (Root/Worker/Plan/Advisor/Subagent) is the only terminal
  outcome type; `PlanOutcome`, `WorkItemOutcome`, `AttemptOutcome`,
  `ExecutionTaskOutcome`, and `Task.outcomes` do not exist.
- `TaskOutcome` carries no `status`/`is_success`; attempt/iteration/workflow
  pass-fail derives from lifecycle enums only.
- `submit_worker_outcome` records `TaskOutcome::Worker` and updates the worker task
  status; `submit_plan_outcome` records `TaskOutcome::Plan` on the planner task.
- Worker task ids derive from `(AttemptId, WorkItemId)`; planner task id derives
  from `AttemptId`; no worker-task id list is persisted.
- `MaterializedPlan` and `PlanDisposition` do not exist; the attempt plan lives in
  the planner task's `TaskOutcome::Plan`.
- Worker context contains `plan_spec`, the current work item, and direct needs
  outcomes; planner context contains the current iteration scope and exactly one
  compact prior-evidence group.
- Terminal result metadata contains no `disposition` and no `submission_kind`.
- The context layer is three files; `ids.rs` and `state/projections.rs` no longer
  exist.
```

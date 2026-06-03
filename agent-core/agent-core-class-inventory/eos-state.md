# Crate `eos-state` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-state/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**33 types across 10 files.**

The `eos-state` crate owns the pure agent-core domain state: the persisted entity
DTOs, the terminal-submission contracts, the typed outcome-projection algebra, and
the per-entity async `Store` traits. It defines *what is stored and what shapes flow
between layers* and never executes I/O. The central entity types are `Request`,
`Task`, `Workflow`, `Iteration`, `Attempt`, `AgentRun`, and `ModelRegistration`,
each an immutable view of a persisted row; their lifecycle vocabularies live in the
paired enums (`TaskStatus`/`TaskRole`, `WorkflowStatus`, `IterationStatus`,
`AttemptStage`/`AttemptStatus`, etc.). The terminal tools ↔ workflow contract is
carried by the validated submission DTOs (`PlannerSubmission`,
`PlannerFailureSubmission`, `GeneratorSubmission`, `ReducerSubmission`), and the
pure projection algebra in `outcomes.rs` (`ExecutionTaskOutcome` plus free
functions) ports `workflow/_core/outcomes.py`. Persistence is abstracted behind
seven ISP-split `#[async_trait]` traits (`WorkflowStore`, `TaskStore`,
`IterationStore`, `AttemptStore`, `RequestStore`, `AgentRunStore`, `ModelStore`),
all sealed via the `Sealed` marker and returning `StoreError` (an alias for
`eos_types::CoreError`). It re-exports value primitives from `eos-types` (the only
upstream dependency it leans on for ids and `CoreError`), sits upstream of
`eos-agent-def`, and is the domain contract that `eos-db` implements and that
`eos-tools`/`eos-engine`/`eos-workflow`/`eos-runtime` consume.

## Contents

- **`eos-state/src/lib.rs`** — _(no inventoried types; module wiring and re-exports only)_
- **`eos-state/src/agent_run.rs`** — `AgentRun`
- **`eos-state/src/attempt.rs`** — `AttemptStage`, `AttemptStatus`, `AttemptFailReason`, `Attempt`
- **`eos-state/src/iteration.rs`** — `IterationStatus`, `IterationCreationReason`, `Iteration`
- **`eos-state/src/model.rs`** — `ModelRegistration`
- **`eos-state/src/outcomes.rs`** — `TaskOutcomeStatus`, `ExecutionRole`, `ExecutionTaskOutcome`
- **`eos-state/src/request.rs`** — `Request`
- **`eos-state/src/store.rs`** — `StoreError`, `Sealed`, `WorkflowStore`, `TaskStore`, `IterationStore`, `AttemptStore`, `RequestStore`, `AgentRunStore`, `ModelStore`
- **`eos-state/src/submissions.rs`** — `PlannerKind`, `PlannerFailReason`, `PlannerSubmission`, `PlannerFailureSubmission`, `GeneratorSubmission`, `ReducerSubmission`
- **`eos-state/src/task.rs`** — `TaskStatus`, `TaskRole`, `Task`
- **`eos-state/src/workflow.rs`** — `WorkflowStatus`, `Workflow`

---

## `eos-state/src/agent_run.rs`

#### `AgentRun`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L14]

Immutable view of a persisted agent-run row — one agent execution for one task (Python `AgentRunRecord`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `AgentRunId` | `pub` |
| `task_id` | `TaskId` | `pub` |
| `initial_messages` | `Option<Vec<JsonObject>>` | `pub` |
| `agent_name` | `String` | `pub` |
| `message_history` | `Option<Vec<JsonObject>>` | `pub` |
| `terminal_tool_result` | `Option<JsonObject>` | `pub` |
| `token_count` | `i64` | `pub` |
| `error` | `Option<String>` | `pub` |
| `created_at` | `UtcDateTime` | `pub` |
| `finished_at` | `Option<UtcDateTime>` | `pub` |

---

## `eos-state/src/attempt.rs`

#### `AttemptStage`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L17]

Stage of an `Attempt` (Python `AttemptStage`); serializes snake_case.

**Variants**: `Plan`, `Run`, `Closed`

#### `AttemptStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L29]

Outcome status of an `Attempt` (Python `AttemptStatus`); serializes snake_case.

**Variants**: `Running`, `Passed`, `Failed`

#### `AttemptFailReason`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L42]

Why an attempt failed (Python `AttemptFailReason`); distinct from `PlannerFailReason`.

**Variants**: `TaskFailed`, `StartupFailed`

#### `Attempt`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L51]

Immutable view of a persisted Attempt — one planner-authored plan (a DAG of generator + reducer tasks) whose reducer set is the exit gate.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `AttemptId` | `pub` |
| `iteration_id` | `IterationId` | `pub` |
| `workflow_id` | `WorkflowId` | `pub` |
| `attempt_sequence_no` | `i64` | `pub` |
| `stage` | `AttemptStage` | `pub` |
| `status` | `AttemptStatus` | `pub` |
| `planner_task_id` | `Option<TaskId>` | `pub` |
| `generator_task_ids` | `Vec<TaskId>` | `pub` |
| `reducer_task_ids` | `Vec<TaskId>` | `pub` |
| `deferred_goal_for_next_iteration` | `Option<String>` | `pub` |
| `fail_reason` | `Option<AttemptFailReason>` | `pub` |
| `created_at` | `UtcDateTime` | `pub` |
| `updated_at` | `UtcDateTime` | `pub` |
| `closed_at` | `Option<UtcDateTime>` | `pub` |
| `outcomes` | `Vec<ExecutionTaskOutcome>` | `pub` |

<details><summary>Methods (1)</summary>

`is_closed`

</details>

---

## `eos-state/src/iteration.rs`

#### `IterationStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L13]

Lifecycle status of an `Iteration` (Python `IterationStatus`); serializes snake_case.

**Variants**: `Open`, `Succeeded`, `Failed`, `Cancelled`

#### `IterationCreationReason`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L27]

Why an iteration was created (Python `IterationCreationReason`); serializes snake_case.

**Variants**: `Initial`, `DeferredGoalContinuation`

#### `Iteration`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L36]

Immutable view of a persisted Iteration (the vertical-continuation axis; Python `state.py:Iteration`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `IterationId` | `pub` |
| `workflow_id` | `WorkflowId` | `pub` |
| `sequence_no` | `i64` | `pub` |
| `creation_reason` | `IterationCreationReason` | `pub` |
| `iteration_goal` | `String` | `pub` |
| `attempt_budget` | `i64` | `pub` |
| `status` | `IterationStatus` | `pub` |
| `attempt_ids` | `Vec<AttemptId>` | `pub` |
| `deferred_goal_for_next_iteration` | `Option<String>` | `pub` |
| `created_at` | `UtcDateTime` | `pub` |
| `updated_at` | `UtcDateTime` | `pub` |
| `closed_at` | `Option<UtcDateTime>` | `pub` |
| `outcomes` | `Option<String>` | `pub` |

<details><summary>Methods (4)</summary>

`is_open`, `attempt_count`, `has_budget_remaining`, `latest_attempt_id`

</details>

---

## `eos-state/src/model.rs`

#### `ModelRegistration`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L15]

Immutable view of a persisted model registration (Python `ModelRegistrationRecord`); `class_path` survives only as migration data and never drives dispatch.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `i64` | `pub` |
| `model_key` | `String` | `pub` |
| `label` | `String` | `pub` |
| `class_path` | `String` | `pub` |
| `kwargs_json` | `String` | `pub` |
| `is_active` | `bool` | `pub` |
| `created_at` | `UtcDateTime` | `pub` |
| `updated_at` | `UtcDateTime` | `pub` |

---

## `eos-state/src/outcomes.rs`

#### `TaskOutcomeStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L21]

Binary status of one execution outcome (Python `TaskOutcomeStatus`); serializes snake_case.

**Variants**: `Success`, `Failed`

#### `ExecutionRole`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L32]

The execution role an outcome belongs to — only generator/reducer evidence appears (Python `ExecutionRole`); serializes snake_case.

**Variants**: `Generator`, `Reducer`

#### `ExecutionTaskOutcome`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L42]

One generator/reducer task's terminal execution evidence, bounded to a single persisted task (Python `ExecutionTaskOutcome`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `status` | `TaskOutcomeStatus` | `pub` |
| `role` | `ExecutionRole` | `pub` |
| `task_id` | `TaskId` | `pub` |
| `outcome` | `String` | `pub` |

---

## `eos-state/src/request.rs`

#### `Request`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L10]

Immutable view of a persisted request row — one top-level user request (Python `RequestRecord`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `RequestId` | `pub` |
| `cwd` | `String` | `pub` |
| `sandbox_id` | `Option<SandboxId>` | `pub` |
| `request_prompt` | `String` | `pub` |
| `root_task_id` | `Option<TaskId>` | `pub` |
| `status` | `String` | `pub` |
| `created_at` | `UtcDateTime` | `pub` |
| `updated_at` | `UtcDateTime` | `pub` |
| `finished_at` | `Option<UtcDateTime>` | `pub` |

---

## `eos-state/src/store.rs`

#### `StoreError`  ·  _type alias_  ·  = `CoreError`  ·  [L30]

Alias for the error every `Store` method returns (`eos_types::CoreError`; no crate-local error enum).

#### `Sealed`  ·  _trait_  ·  [L40]

Sealing marker for the `Store` traits; `#[doc(hidden)]`, implemented only by the `eos-db` repositories and in-crate test fakes.

**Trait items**: _(none — marker trait)_

#### `WorkflowStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L44]

Persistence surface for `Workflow` (Python `WorkflowStoreProtocol`).

**Trait items**:
- `async fn insert(&self, request_id: &RequestId, parent_task_id: &TaskId, workflow_goal: &str) -> Result<Workflow, CoreError>;`
- `async fn get(&self, id: &WorkflowId) -> Result<Option<Workflow>, CoreError>;`
- `async fn append_iteration_id(&self, id: &WorkflowId, iteration_id: &IterationId) -> Result<Workflow, CoreError>;`
- `async fn set_status(&self, id: &WorkflowId, status: WorkflowStatus, closed_at: Option<UtcDateTime>, outcomes: Option<&str>) -> Result<Workflow, CoreError>;`
- `async fn list_for_parent_task(&self, parent_task_id: &TaskId) -> Result<Vec<Workflow>, CoreError>;`

#### `TaskStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L82]

Persistence surface for request/task (Python `TaskStoreProtocol`, task half).

**Trait items**:
- `async fn upsert_task(&self, task: &Task) -> Result<(), CoreError>;`
- `async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError>;`
- `async fn set_task_status(&self, id: &TaskId, status: TaskStatus, outcomes: Option<&[ExecutionTaskOutcome]>, terminal_tool_result: Option<&JsonObject>) -> Result<Task, CoreError>;`
- `async fn set_task_status_if_current(&self, id: &TaskId, expected: TaskStatus, status: TaskStatus, outcomes: Option<&[ExecutionTaskOutcome]>, terminal_tool_result: Option<&JsonObject>) -> Result<Option<Task>, CoreError>;`

#### `IterationStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L113]

Persistence surface for `Iteration` (Python `IterationStoreProtocol`).

**Trait items**:
- `async fn insert(&self, workflow_id: &WorkflowId, sequence_no: i64, creation_reason: IterationCreationReason, iteration_goal: &str, attempt_budget: i64) -> Result<Iteration, CoreError>;`
- `async fn get(&self, id: &IterationId) -> Result<Option<Iteration>, CoreError>;`
- `async fn append_attempt_id(&self, id: &IterationId, attempt_id: &AttemptId) -> Result<Iteration, CoreError>;`
- `async fn set_status(&self, id: &IterationId, status: IterationStatus, closed_at: Option<UtcDateTime>, outcomes: Option<&str>) -> Result<Iteration, CoreError>;`
- `async fn set_deferred_goal_for_next_iteration(&self, id: &IterationId, deferred_goal_for_next_iteration: Option<&str>) -> Result<Iteration, CoreError>;`
- `async fn close_succeeded(&self, id: &IterationId, outcomes: &str, closed_at: Option<UtcDateTime>) -> Result<Iteration, CoreError>;`
- `async fn list_for_workflow(&self, workflow_id: &WorkflowId) -> Result<Vec<Iteration>, CoreError>;`

#### `AttemptStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L168]

Persistence surface for `Attempt` (Python `AttemptStoreProtocol`).

**Trait items**:
- `async fn insert(&self, iteration_id: &IterationId, workflow_id: &WorkflowId, attempt_sequence_no: i64) -> Result<Attempt, CoreError>;`
- `async fn get(&self, id: &AttemptId) -> Result<Option<Attempt>, CoreError>;`
- `async fn set_stage(&self, id: &AttemptId, stage: AttemptStage) -> Result<Attempt, CoreError>;`
- `async fn set_planner_task_id(&self, id: &AttemptId, planner_task_id: &TaskId) -> Result<Attempt, CoreError>;`
- `async fn set_generator_task_ids(&self, id: &AttemptId, generator_task_ids: &[TaskId]) -> Result<Attempt, CoreError>;`
- `async fn set_reducer_task_ids(&self, id: &AttemptId, reducer_task_ids: &[TaskId]) -> Result<Attempt, CoreError>;`
- `async fn set_deferred_goal(&self, id: &AttemptId, deferred_goal_for_next_iteration: Option<&str>) -> Result<Attempt, CoreError>;`
- `async fn close(&self, id: &AttemptId, status: AttemptStatus, fail_reason: Option<AttemptFailReason>, outcomes: Option<&[ExecutionTaskOutcome]>, closed_at: UtcDateTime) -> Result<Attempt, CoreError>;`
- `async fn list_for_iteration(&self, iteration_id: &IterationId) -> Result<Vec<Attempt>, CoreError>;`

#### `RequestStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L232]

Persistence surface for requests (Python `TaskStoreProtocol`, request half, split out per ISP).

**Trait items**:
- `async fn create_request(&self, request_id: &RequestId, cwd: &str, sandbox_id: Option<&SandboxId>, request_prompt: &str) -> Result<(), CoreError>;`
- `async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError>;`
- `async fn set_root_task_id(&self, id: &RequestId, root_task_id: &TaskId) -> Result<Request, CoreError>;`
- `async fn finish_request(&self, id: &RequestId, status: &str) -> Result<Option<Request>, CoreError>;`

#### `AgentRunStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L264]

Persistence surface for `AgentRun` (Python `AgentRunStore`).

**Trait items**:
- `async fn create_run(&self, agent_run_id: &AgentRunId, task_id: &TaskId, agent_name: &str, initial_messages: Option<&[JsonObject]>) -> Result<AgentRun, CoreError>;`
- `async fn finish_run(&self, agent_run_id: &AgentRunId, message_history: Option<&[JsonObject]>, terminal_tool_result: Option<&JsonObject>, token_count: i64, error: Option<&str>) -> Result<Option<AgentRun>, CoreError>;`
- `async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError>;`

#### `ModelStore`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L290]

Persistence surface for `ModelRegistration` (Python `ModelStore`).

**Trait items**:
- `async fn register(&self, model_key: &str, label: &str, class_path: &str, kwargs: &JsonObject, activate: bool) -> Result<ModelRegistration, CoreError>;`
- `async fn delete(&self, model_key: &str) -> Result<bool, CoreError>;`
- `async fn get(&self, model_key: &str) -> Result<Option<ModelRegistration>, CoreError>;`
- `async fn active(&self) -> Result<Option<ModelRegistration>, CoreError>;`

---

## `eos-state/src/submissions.rs`

#### `PlannerKind`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L17]

Whether a planner submission completes the attempt or defers a goal (Python `Literal["completes","defers"]`); serializes snake_case.

**Variants**: `Completes`, `Defers`

#### `PlannerFailReason`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L28]

Why a planner submission failed (Python `Literal["run_exhausted"]`); distinct from `AttemptFailReason`.

**Variants**: `RunExhausted`

#### `PlannerSubmission`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L36]

Validated planner submission from a full or partial plan tool (Python `PlannerSubmission`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `attempt_id` | `AttemptId` | `pub` |
| `planner_task_id` | `TaskId` | `pub` |
| `kind` | `PlannerKind` | `pub` |
| `generator_task_ids` | `Vec<TaskId>` | `pub` |
| `reducer_task_ids` | `Vec<TaskId>` | `pub` |
| `deferred_goal_for_next_iteration` | `Option<String>` | `pub` |

#### `PlannerFailureSubmission`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L53]

Runtime-synthesized planner failure (Python `PlannerFailureSubmission`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `attempt_id` | `AttemptId` | `pub` |
| `planner_task_id` | `TaskId` | `pub` |
| `fail_reason` | `PlannerFailReason` | `pub` |

#### `GeneratorSubmission`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L64]

Validated terminal outcome for one generator task (Python `GeneratorSubmission`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `attempt_id` | `AttemptId` | `pub` |
| `task_id` | `TaskId` | `pub` |
| `status` | `TaskOutcomeStatus` | `pub` |
| `outcome` | `String` | `pub` |
| `terminal_tool_result` | `JsonObject` | `pub` |

#### `ReducerSubmission`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L79]

Validated terminal outcome for one reducer task (Python `ReducerSubmission`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `attempt_id` | `AttemptId` | `pub` |
| `task_id` | `TaskId` | `pub` |
| `status` | `TaskOutcomeStatus` | `pub` |
| `outcome` | `String` | `pub` |
| `terminal_tool_result` | `JsonObject` | `pub` |

---

## `eos-state/src/task.rs`

#### `TaskStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L17]

Lifecycle status of a persisted `Task` (Python `TaskStatus`); serializes snake_case.

**Variants**: `Pending`, `Running`, `Done`, `Failed`, `Blocked`

<details><summary>Methods (1)</summary>

`is_terminal_generator`

</details>

#### `TaskRole`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L44]

The four persisted task roles (Python `TASK_AGENT_ROLES`); the execution role is `Generator`, no profile-alias role enters persisted state. Serializes snake_case.

**Variants**: `Root`, `Planner`, `Generator`, `Reducer`

#### `Task`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L65]

Immutable view of a persisted task — the persisted agent interface (Python `task/task.py:Task`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `TaskId` | `pub` |
| `request_id` | `RequestId` | `pub` |
| `role` | `TaskRole` | `pub` |
| `instruction` | `String` | `pub` |
| `status` | `TaskStatus` | `pub` |
| `workflow_id` | `Option<WorkflowId>` | `pub` · `#[serde(default)]` |
| `iteration_id` | `Option<IterationId>` | `pub` · `#[serde(default)]` |
| `attempt_id` | `Option<AttemptId>` | `pub` · `#[serde(default)]` |
| `agent_name` | `Option<String>` | `pub` · `#[serde(default)]` |
| `needs` | `Vec<TaskId>` | `pub` · `#[serde(default)]` |
| `outcomes` | `Vec<ExecutionTaskOutcome>` | `pub` · `#[serde(default)]` |
| `terminal_tool_result` | `Option<JsonObject>` | `pub` · `#[serde(default)]` |

---

## `eos-state/src/workflow.rs`

#### `WorkflowStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  [L14]

Lifecycle status of a `Workflow` (Python `WorkflowStatus`); serializes snake_case.

**Variants**: `Open`, `Succeeded`, `Failed`, `Cancelled`

#### `Workflow`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L27]

Immutable view of a persisted Workflow (the origin axis; Python `state.py:Workflow`); `parent_task_id` is a durable back-link never mutated at close.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `WorkflowId` | `pub` |
| `request_id` | `RequestId` | `pub` |
| `workflow_goal` | `String` | `pub` |
| `status` | `WorkflowStatus` | `pub` |
| `iteration_ids` | `Vec<IterationId>` | `pub` |
| `parent_task_id` | `TaskId` | `pub` |
| `outcomes` | `Option<String>` | `pub` |
| `created_at` | `UtcDateTime` | `pub` |
| `updated_at` | `UtcDateTime` | `pub` |
| `closed_at` | `Option<UtcDateTime>` | `pub` |

<details><summary>Methods (1)</summary>

`is_open`

</details>

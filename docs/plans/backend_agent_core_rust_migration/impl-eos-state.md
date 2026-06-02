# impl-eos-state — pure agent-core domain state, outcome projections, terminal submission DTOs, and per-entity async store traits

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §2 (also §"SRP, Naming, and Prompt Gaps to Close").

## 1. Purpose & Responsibility (SRP)

`eos-state` is the **pure domain layer** for agent-core. It owns the persisted
entity DTOs (`Task`, `Workflow`, `Iteration`, `Attempt`), their lifecycle enums,
the outcome record type with its pure projection functions, the validated
terminal submission DTOs that flow tools → workflow, and the **per-entity async
`Store` traits** (`WorkflowStore`, `IterationStore`, `AttemptStore`, `TaskStore`,
`RequestStore`, `AgentRunStore`, `ModelStore`). It is the upstream contract that
`eos-db` implements and that `eos-tools`, `eos-engine`, `eos-workflow`, and
`eos-runtime` consume.

This crate **must NOT**: open a database, build SQL, parse SQLite rows, run
migrations, perform HTTP, touch sandbox/provider code, redact secrets, parse
model `kwargs_json`, build context packets, or own lifecycle policy. It defines
*what is stored and what shapes flow between layers*; it never *executes* I/O.
All `Store` methods are `async fn` declarations only — the concrete sync/sqlx
implementations live in `eos-db`. Mutually-exclusive states are enums
(`type-enum-states`); nullable fields are `Option<T>` (`type-option-nullable`);
all I/O-shaped trait methods return `Result<_, CoreError>` (`type-result-fallible`).

## 2. Dependencies

- **Upstream crates (depends on):** `eos-types` (newtype IDs, `UtcDateTime`,
  `Clock`, `CoreError`, `JsonObject` — see impl-eos-types.md / anchor §5). This is
  the only intra-workspace dependency; `eos-state` is one hop above the leaf.
- **Downstream consumers (used by):** `eos-db` (implements every `Store` trait),
  `eos-tools`, `eos-engine`, `eos-workflow`, `eos-runtime`.
- **External crates** (all pinned via `[workspace.dependencies]` and inherited
  with `foo = { workspace = true }`, `proj-workspace-deps`):

  | Crate | Use | Justification / rust-skills |
  |---|---|---|
  | `serde` (derive) | `Serialize`/`Deserialize` on every DTO/enum (wire + JSONL round-trip) | DTO contracts cross layers; `api-common-traits` |
  | `schemars` | `JsonSchema` on submission DTOs + outcome records for Phase-0 schema parity snapshots | anchor §11 parity harness; spec §9 |
  | `async-trait` | object-safe `async fn` in the `Store` traits behind `Arc<dyn ...>` at the composition root | anchor §6 object-safety note; `test-mock-traits` |
  | `thiserror` | this crate adds **no new error enum** (see §8); it re-exports/returns `eos-types::CoreError`. `thiserror` is pulled in only if a thin local variant is later required — default: none | `err-thiserror-lib`, `err-custom-type` |

  `serde_json` is **not** a direct dependency: JSON (de)serialization of the
  `outcomes` string field is owned by `eos-db` at the persistence boundary. The
  pure projection functions in `outcomes.rs` operate on already-parsed
  `Vec<ExecutionTaskOutcome>` / `Option<&str>`-free typed inputs (see §6/§8).

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `backend/src/task/task.py` | `task.rs` | `Task` DTO + `TaskStatus`; `TASK_AGENT_ROLES` / `TERMINAL_GENERATOR_STATUSES` become `const` sets / helper fns. `role: AgentRole` is referenced from `eos-agent-def`? **No** — to keep `eos-state` upstream of `eos-agent-def`, `Task.role` is a local `TaskRole` enum (the 4 persisted task roles); see §6/§8 ownership note. |
| `backend/src/workflow/_core/state.py` | `workflow.rs`, `iteration.rs`, `attempt.rs` | `Workflow`/`Iteration`/`Attempt` DTOs + `WorkflowStatus`, `IterationStatus`, `IterationCreationReason`, `AttemptStage`, `AttemptStatus`, `AttemptFailReason`; `is_open`/`attempt_count`/`has_budget_remaining`/`latest_attempt_id`/`is_closed` become methods. |
| `backend/src/workflow/_core/outcomes.py` | `outcomes.rs` | `ExecutionTaskOutcome` + `TaskOutcomeStatus`/`ExecutionRole` enums + pure projections. JSON parse/serialize helpers (`parse_outcomes_record`, `records_json`, `to_record`) move to `eos-db`; only the **typed** projection algebra stays here (see §6). |
| `backend/src/workflow/_core/persistence.py` | `store.rs` | `*StoreProtocol` Protocols → `#[async_trait]` per-entity traits returning typed DTOs. `TaskRow = dict[str,Any]` is **dropped**: `TaskStore` returns `Option<Task>` (typed), closing the plan's "stop serializing id twice" gap. `is_ready: bool` → readiness is an `eos-db` concern, not a trait member. |
| `backend/src/workflow/submissions.py` | `submissions.rs` | `PlannerSubmission`, `PlannerFailureSubmission`, `GeneratorSubmission`, `ReducerSubmission` (1:1, with `Literal[...]` → enums). |
| `backend/src/workflow/_core/persistence.py` `TaskStoreProtocol` (request surface) + `db/models/request.py` (`RequestRecord` columns) + `db/stores/task_store.py` impl | `store.rs` (`RequestStore`) + `request.rs` (`Request` DTO) | `RequestStore` split out of the Python `TaskStoreProtocol` (ISP) — there is no standalone request-store class; request CRUD lives on `TaskStoreProtocol`. `Request` DTO captures real columns. |
| `backend/src/db/models/agent_run.py`, `db/stores/agent_run_store.py` | `store.rs` (`AgentRunStore`) + `agent_run.rs` (`AgentRun` DTO) | new domain DTO + trait; fields below are real. |
| `backend/src/db/models/model_registration.py`, `db/stores/model_store.py` | `store.rs` (`ModelStore`) + `model.rs` (`ModelRegistration` DTO) | `class_path`/`kwargs_json` kept as **opaque migration-only** fields; normalized `model_key` exposed at the domain boundary (`llm_provider` is **derived in `eos-db`**, not an `eos-state` field — see §6.7) (anchor §4, non-goal: no `class_path` dispatch). Secret redaction/env-resolution stays in `eos-db`. |

**In scope:** entity DTOs, lifecycle enums, outcome algebra, submission DTOs,
store trait declarations.
**Out of scope (non-goals, anchor §2):** SQL/sqlx, JSON string (de)serialization,
secret redaction, `kwargs` parsing, `class_path` dynamic dispatch, context
packets, any orchestration/lifecycle decision.

## 4. File & Module Layout

```
src/
  lib.rs          // pub use re-exports of every public DTO/enum/trait (proj-pub-use-reexport)
  task.rs         // Task, TaskStatus, TaskRole (task-row role mirror), role/status helper consts
  workflow.rs     // Workflow, WorkflowStatus
  iteration.rs    // Iteration, IterationStatus, IterationCreationReason
  attempt.rs      // Attempt, AttemptStage, AttemptStatus, AttemptFailReason
  request.rs      // Request DTO (top-level request row, typed)
  agent_run.rs    // AgentRun DTO
  model.rs        // ModelRegistration DTO (migration-shaped) + normalized accessors
  outcomes.rs     // ExecutionTaskOutcome, TaskOutcomeStatus, ExecutionRole, pure projections
  submissions.rs  // PlannerSubmission, PlannerFailureSubmission, GeneratorSubmission, ReducerSubmission
  store.rs        // the 7 #[async_trait] Store traits (sealed); StoreError alias = CoreError
```

`lib.rs` re-exports the public surface (`pub use`); cross-file internal helpers
in `outcomes.rs` (e.g. the record-normalization fallback) are `pub(crate)`
(`proj-pub-crate-internal`). No `prelude` module — the flat re-export is enough
(`proj-flat-small`).

## 5. Contracts Owned Here

Per the Ownership Map (anchor §5), this crate **owns**: the domain state DTOs and
their enums, the outcome projections, the terminal submission DTOs, and the
per-entity `Store` traits. Everything below is fully specified here; downstream
docs reference it.

### 5.1 Per-entity `Store` traits (ISP — no god-store)

Seven small traits, one per persisted entity, split out of Python's four
`*StoreProtocol`s (`TaskStoreProtocol` carried both request and task surfaces —
split into `RequestStore` + `TaskStore`; `AgentRunStore`/`ModelStore` are
promoted from `db.stores` per the module directive). Each trait lists **only the
methods agent-core actually calls** (matching the Python "narrow contract"
discipline). All methods are `async fn` and return `Result<_, CoreError>`.

- **Object safety / async:** all seven traits are used behind `Arc<dyn Trait>` in
  the `eos-runtime` composition root (heterogeneous storage), so they carry
  `#[async_trait]` (native async-fn-in-trait is not yet `dyn`-safe; anchor §6).
- **Sealed:** each trait extends a crate-private `Sealed` marker
  (`api-sealed-trait`) so only `eos-db` repos and in-crate test fakes implement
  them; the contract can gain methods without a breaking change for external
  crates.
- **Bounds:** `Send + Sync` so they cross the Tokio multi-thread runtime
  (`test-mock-traits`).

Signature sketch — all seven traits below (the single `use` block covers every
trait; §6.9 maps each method back to its Python protocol source):

```rust
use async_trait::async_trait;
use eos_types::{
    AgentRunId, AttemptId, CoreError, IterationId, JsonObject, RequestId, SandboxId,
    TaskId, UtcDateTime, WorkflowId,
};

mod sealed {
    pub trait Sealed {}
}

#[async_trait]
pub trait WorkflowStore: sealed::Sealed + Send + Sync {
    async fn insert(
        &self,
        request_id: &RequestId,
        parent_task_id: &TaskId,
        workflow_goal: &str,            // own-slice-over-vec / anti-string-for-str
    ) -> Result<Workflow, CoreError>;
    async fn get(&self, id: &WorkflowId) -> Result<Option<Workflow>, CoreError>;
    async fn append_iteration_id(
        &self,
        id: &WorkflowId,
        iteration_id: &IterationId,
    ) -> Result<Workflow, CoreError>;
    async fn set_status(
        &self,
        id: &WorkflowId,
        status: WorkflowStatus,
        closed_at: Option<UtcDateTime>,
        outcomes: Option<&str>,          // serialized projection; None = leave unchanged
    ) -> Result<Workflow, CoreError>;
    async fn list_for_parent_task(
        &self,
        parent_task_id: &TaskId,
    ) -> Result<Vec<Workflow>, CoreError>;
}

#[async_trait]
pub trait TaskStore: sealed::Sealed + Send + Sync {
    async fn upsert_task(&self, task: &Task) -> Result<(), CoreError>;
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError>;
    async fn set_task_status(
        &self,
        id: &TaskId,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Task, CoreError>;
    /// Optimistic-concurrency status flip (Python `set_task_status_if_current`).
    /// `Ok(None)` ⇒ current status did not match `expected`.
    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError>;
}

#[async_trait]
pub trait IterationStore: sealed::Sealed + Send + Sync {
    async fn insert(
        &self,
        workflow_id: &WorkflowId,
        sequence_no: i64,
        creation_reason: IterationCreationReason,
        iteration_goal: &str,
        attempt_budget: i64,
    ) -> Result<Iteration, CoreError>;
    async fn get(&self, id: &IterationId) -> Result<Option<Iteration>, CoreError>;
    async fn append_attempt_id(
        &self,
        id: &IterationId,
        attempt_id: &AttemptId,
    ) -> Result<Iteration, CoreError>;
    async fn set_status(
        &self,
        id: &IterationId,
        status: IterationStatus,
        closed_at: Option<UtcDateTime>,
        outcomes: Option<&str>,          // serialized projection; None = leave unchanged
    ) -> Result<Iteration, CoreError>;
    async fn set_deferred_goal_for_next_iteration(
        &self,
        id: &IterationId,
        deferred_goal_for_next_iteration: Option<&str>,
    ) -> Result<Iteration, CoreError>;
    async fn close_succeeded(
        &self,
        id: &IterationId,
        outcomes: &str,
        closed_at: Option<UtcDateTime>,
    ) -> Result<Iteration, CoreError>;
    async fn list_for_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Iteration>, CoreError>;
}

#[async_trait]
pub trait AttemptStore: sealed::Sealed + Send + Sync {
    async fn insert(
        &self,
        iteration_id: &IterationId,
        workflow_id: &WorkflowId,
        attempt_sequence_no: i64,
    ) -> Result<Attempt, CoreError>;
    async fn get(&self, id: &AttemptId) -> Result<Option<Attempt>, CoreError>;
    async fn set_stage(&self, id: &AttemptId, stage: AttemptStage) -> Result<Attempt, CoreError>;
    async fn set_planner_task_id(
        &self,
        id: &AttemptId,
        planner_task_id: &TaskId,
    ) -> Result<Attempt, CoreError>;
    async fn set_generator_task_ids(
        &self,
        id: &AttemptId,
        generator_task_ids: &[TaskId],
    ) -> Result<Attempt, CoreError>;
    async fn set_reducer_task_ids(
        &self,
        id: &AttemptId,
        reducer_task_ids: &[TaskId],
    ) -> Result<Attempt, CoreError>;
    async fn set_deferred_goal(
        &self,
        id: &AttemptId,
        deferred_goal_for_next_iteration: Option<&str>,
    ) -> Result<Attempt, CoreError>;
    /// Python `close`: status / fail_reason / outcomes / closed_at.
    async fn close(
        &self,
        id: &AttemptId,
        status: AttemptStatus,
        fail_reason: Option<AttemptFailReason>,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        closed_at: UtcDateTime,
    ) -> Result<Attempt, CoreError>;
    async fn list_for_iteration(
        &self,
        iteration_id: &IterationId,
    ) -> Result<Vec<Attempt>, CoreError>;
}

#[async_trait]
pub trait RequestStore: sealed::Sealed + Send + Sync {
    async fn create_request(
        &self,
        request_id: &RequestId,
        cwd: &str,
        sandbox_id: Option<&SandboxId>,
        request_prompt: &str,
    ) -> Result<(), CoreError>;                          // Python returns None
    async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError>;
    async fn set_root_task_id(
        &self,
        id: &RequestId,
        root_task_id: &TaskId,
    ) -> Result<Request, CoreError>;
    /// `status` is the free-form request-status string (default `"running"` is
    /// set by `create_request` in `eos-db`); the vocabulary is not enumerated here.
    /// `finish_request` stamps `finished_at` server-side in `eos-db` (mirroring the
    /// Python set-now-at-finish), so the trait carries no `finished_at` argument.
    async fn finish_request(
        &self,
        id: &RequestId,
        status: &str,
    ) -> Result<Option<Request>, CoreError>;
}

#[async_trait]
pub trait AgentRunStore: sealed::Sealed + Send + Sync {
    async fn create_run(
        &self,
        agent_run_id: &AgentRunId,
        task_id: &TaskId,
        agent_name: &str,
        initial_messages: Option<&[JsonObject]>,
    ) -> Result<AgentRun, CoreError>;
    async fn finish_run(
        &self,
        agent_run_id: &AgentRunId,
        message_history: Option<&[JsonObject]>,
        terminal_tool_result: Option<&JsonObject>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<AgentRun>, CoreError>;
    async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError>;
}

#[async_trait]
pub trait ModelStore: sealed::Sealed + Send + Sync {
    /// Python `register` params (`activate` is not a DTO field; `id`/timestamps are
    /// server-set). `kwargs` is passed as a `&JsonObject`; the `json.dumps` →
    /// `kwargs_json` and any redaction are `eos-db`'s job (§6.7, §2 — no `serde_json`
    /// here, only the type name).
    async fn register(
        &self,
        model_key: &str,
        label: &str,
        class_path: &str,
        kwargs: &JsonObject,
        activate: bool,
    ) -> Result<ModelRegistration, CoreError>;
    async fn delete(&self, model_key: &str) -> Result<bool, CoreError>;
    async fn get(&self, model_key: &str) -> Result<Option<ModelRegistration>, CoreError>;
    async fn active(&self) -> Result<Option<ModelRegistration>, CoreError>;
}
```

(`upsert_task` takes a typed `&Task` rather than the Python keyword soup — the
DTO already carries every column, which removes the `id`/`task_id` double-serialize
gap. Python's `is_ready: bool` protocol attribute is **dropped** from every trait —
readiness is an `eos-db` concern, not a contract member (§3). §6.9 cross-maps each
method above to its Python protocol source.)

### 5.2 Outcome projections (pure functions — owned here)

`outcomes.rs` owns the projection algebra as **pure async-free functions**
(no `self`, no I/O), taking `Option<&dyn TaskStore>` where the Python took
`TaskStoreProtocol | None`. Because the projections call `task_store.get`
(async), the projection fns that touch the store are `async`; the in-memory ones
(`execution_outcome_for_submission`, `present_status`, `latest_iteration`) are
sync. See §6.8.

`latest_iteration(iterations: &[Iteration]) -> Option<&Iteration>` is the pure
argmax-by-`sequence_no` selector that Python's `workflow_outcomes` performs before
parsing. It stays here (it is a pure domain projection, anchor §5 "outcome
projections"); only the `json.loads` of the chosen iteration's serialized
`outcomes` string moves to `eos-db` (see §6.8).

### 5.3 Submission & state DTOs

All DTOs in §6 (`Task`, `Workflow`, `Iteration`, `Attempt`, `Request`,
`AgentRun`, `ModelRegistration`, `ExecutionTaskOutcome`, the four submissions)
are owned here.

**Contracts merely USED (referenced, not redefined):** `TaskId`, `WorkflowId`,
`IterationId`, `AttemptId`, `RequestId`, `AgentRunId`, `UtcDateTime`,
`Clock`, `CoreError`, `JsonObject` — all owned by `eos-types` (see
impl-eos-types.md / anchor §5).

## 6. Types, Fields & Schemas

Every DTO derives `Debug, Clone, PartialEq` (`api-common-traits`) and
`Serialize, Deserialize, JsonSchema`. Frozen-immutable in Python → plain owned
structs in Rust (the immutability invariant is enforced by the store returning
fresh values, not by interior mutability). Public structs/enums that may gain
fields/variants carry `#[non_exhaustive]` (`api-non-exhaustive`). Enums derive
additionally `Eq, Hash` and use `#[serde(rename_all = "snake_case")]` to match
the Python `StrEnum` wire values.

### 6.1 `Task` (source: `task/task.py`)

| Field | Rust type | serde/schemars | Source-of-truth |
|---|---|---|---|
| `id` | `TaskId` | `#[serde(transparent)]` newtype | `Task.id` |
| `request_id` | `RequestId` | | `Task.request_id` |
| `role` | `AgentRole` | snake_case enum | `Task.role` |
| `instruction` | `String` | | `Task.instruction` |
| `status` | `TaskStatus` | snake_case enum | `Task.status` |
| `workflow_id` | `Option<WorkflowId>` | nullable (`type-option-nullable`) | `Task.workflow_id` |
| `iteration_id` | `Option<IterationId>` | | `Task.iteration_id` |
| `attempt_id` | `Option<AttemptId>` | | `Task.attempt_id` |
| `agent_name` | `Option<String>` | | `Task.agent_name` |
| `needs` | `Vec<TaskId>` | default `[]` | `Task.needs: tuple[str, ...]` |
| `outcomes` | `Vec<ExecutionTaskOutcome>` | default `[]` | Python `Task.outcomes: tuple[Any, ...]` (opaque); tightened to typed `Vec<ExecutionTaskOutcome>` here — the element constraint is a Rust-side narrowing enforced at the `eos-db` parse boundary |
| `terminal_tool_result` | `Option<JsonObject>` | | `Task.terminal_tool_result: dict \| None` |

`TaskStatus` enum (source: `TaskStatus(StrEnum)`): `Pending`, `Running`, `Done`,
`Failed`, `Blocked`.

`TaskRole`: `Root`, `Planner`, `Generator`, `Reducer` —
exactly `TASK_AGENT_ROLES`. **Naming normalization (anchor §4, GC-eos-state-02):**
the execution state role is `Generator`; `executor` never appears here (profile
alias only, owned by `eos-agent-def`). `TASK_AGENT_ROLES` and
`TERMINAL_GENERATOR_STATUSES` become:

```rust
pub const TASK_AGENT_ROLES: [TaskRole; 4] =
    [TaskRole::Root, TaskRole::Planner, TaskRole::Generator, TaskRole::Reducer];

impl TaskStatus {
    /// Python `TERMINAL_GENERATOR_STATUSES`.
    #[must_use]
    pub const fn is_terminal_generator(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Blocked)
    }
}
```

> **Ownership note (GC-eos-state-05):** the *full* `AgentRole`/`AgentType`
> registry is owned by `eos-agent-def` (anchor §5). To keep the dependency DAG
> acyclic (`eos-state` is upstream of `eos-agent-def`), `eos-state` defines the
> minimal 4-variant task-role enum it needs for `Task.role`. `eos-agent-def`
> references this type for its task roles rather than redefining it; non-task
> profile roles (e.g. an `executor` alias) live only in `eos-agent-def`.

### 6.2 `Workflow` (source: `state.py`)

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `id` | `WorkflowId` | | `Workflow.id` |
| `request_id` | `RequestId` | | |
| `workflow_goal` | `String` | normalized name (anchor §4); DB column may be `goal` (mapped in `eos-db`) | `Workflow.workflow_goal` |
| `status` | `WorkflowStatus` | | |
| `iteration_ids` | `Vec<IterationId>` | | `Workflow.iteration_ids` |
| `parent_task_id` | `TaskId` | durable back-link; **never** mutated at close (anchor §3) | `Workflow.parent_task_id` |
| `outcomes` | `Option<String>` | serialized projection; `None` while open | `Workflow.outcomes: str \| None` |
| `created_at` | `UtcDateTime` | | |
| `updated_at` | `UtcDateTime` | | |
| `closed_at` | `Option<UtcDateTime>` | | |

`WorkflowStatus`: `Open`, `Succeeded`, `Failed`, `Cancelled`. Method:
`pub const fn is_open(&self) -> bool` (`status == Open`).

### 6.3 `Iteration` (source: `state.py`)

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `id` | `IterationId` | | |
| `workflow_id` | `WorkflowId` | | |
| `sequence_no` | `i64` | `mem-smaller-integers`: SQLite INTEGER; `i64` matches col | `Iteration.sequence_no: int` |
| `creation_reason` | `IterationCreationReason` | | |
| `iteration_goal` | `String` | normalized (anchor §4); DB col may be `goal` | `Iteration.iteration_goal` |
| `attempt_budget` | `i64` | | |
| `status` | `IterationStatus` | | |
| `attempt_ids` | `Vec<AttemptId>` | | |
| `deferred_goal_for_next_iteration` | `Option<String>` | normalized (anchor §4); DB col may be `deferred_goal` | `Iteration.deferred_goal_for_next_iteration` |
| `created_at`/`updated_at` | `UtcDateTime` | | |
| `closed_at` | `Option<UtcDateTime>` | | |
| `outcomes` | `Option<String>` | serialized projection (`json.dumps` list); `None` while open | `Iteration.outcomes` |

`IterationStatus`: `Open`, `Succeeded`, `Failed`, `Cancelled`.
`IterationCreationReason`: `Initial`, `DeferredGoalContinuation`
(`#[serde(rename = "deferred_goal_continuation")]`). Methods:
`is_open`, `attempt_count() -> usize`, `has_budget_remaining() -> bool`,
`latest_attempt_id() -> Option<&AttemptId>`.

### 6.4 `Attempt` (source: `state.py`)

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `id` | `AttemptId` | | |
| `iteration_id` | `IterationId` | | |
| `workflow_id` | `WorkflowId` | | |
| `attempt_sequence_no` | `i64` | | |
| `stage` | `AttemptStage` | | |
| `status` | `AttemptStatus` | | |
| `planner_task_id` | `Option<TaskId>` | | |
| `generator_task_ids` | `Vec<TaskId>` | the plan's generator set | |
| `reducer_task_ids` | `Vec<TaskId>` | the plan's reducer set (exit gate) | |
| `deferred_goal_for_next_iteration` | `Option<String>` | normalized | |
| `fail_reason` | `Option<AttemptFailReason>` | | |
| `created_at`/`updated_at` | `UtcDateTime` | | |
| `closed_at` | `Option<UtcDateTime>` | | |
| `outcomes` | `Vec<ExecutionTaskOutcome>` | default `[]`; Python `tuple[Any, ...]` (opaque); tightened to typed `Vec<ExecutionTaskOutcome>` here — Rust-side narrowing enforced at the `eos-db` parse boundary | `Attempt.outcomes` |

`AttemptStage`: `Plan`, `Run`, `Closed`. `AttemptStatus`: `Running`, `Passed`,
`Failed`. `AttemptFailReason`: `TaskFailed`, `StartupFailed`. Method:
`pub const fn is_closed(&self) -> bool` (`stage == Closed`).

### 6.5 `Request` (source: `db/models/request.py`)

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `id` | `RequestId` | | `RequestRecord.id` |
| `cwd` | `String` | | `RequestRecord.cwd` |
| `sandbox_id` | `Option<SandboxId>` | `SandboxId` from eos-types | `RequestRecord.sandbox_id` |
| `request_prompt` | `String` | | |
| `root_task_id` | `Option<TaskId>` | root `Task(role=root, workflow_id=None)` link | `RequestRecord.root_task_id` |
| `status` | `String` | request status (`running`/finished); kept as `String` — request status vocabulary is broader than `TaskStatus` and set via `finish_request(status)` | `RequestRecord.status` |
| `created_at`/`updated_at` | `UtcDateTime` | | |
| `finished_at` | `Option<UtcDateTime>` | | |

### 6.6 `AgentRun` (source: `db/models/agent_run.py`)

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `id` | `AgentRunId` | | `AgentRunRecord.id` |
| `task_id` | `TaskId` | 1:1 with task (unique) | |
| `initial_messages` | `Option<Vec<JsonObject>>` | transcript seed; provider-neutral JSON, typed in `eos-llm-client`/`eos-engine` | `AgentRunRecord.initial_messages` |
| `agent_name` | `String` | | |
| `message_history` | `Option<Vec<JsonObject>>` | | |
| `terminal_tool_result` | `Option<JsonObject>` | | |
| `token_count` | `i64` | default `0` (`api-default-impl`) | |
| `error` | `Option<String>` | | |
| `created_at` | `UtcDateTime` | | |
| `finished_at` | `Option<UtcDateTime>` | | |

> `initial_messages`/`message_history` stay as `Vec<JsonObject>` (provider-neutral
> opaque blocks) — typed `Message` modeling is owned by `eos-llm-client`; lifting
> it here would create a downstream dependency and violate the DAG.

### 6.7 `ModelRegistration` (source: `db/models/model_registration.py`)

`class_path` survives **only as migration data** (anchor §2 non-goal): final
dispatch is typed by `llm_provider` + `model_key`. This DTO therefore keeps the
raw migration columns and exposes **normalized accessors**; it carries **no
dispatch logic**.

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `id` | `i64` | autoincrement INTEGER PK | `ModelRegistrationRecord.id` |
| `model_key` | `String` | normalized name (anchor §4); DB column is `key` (mapped in `eos-db`). Stays `String`, not a newtype — `eos-types` GC-types-02 keeps `model_key` untyped | `ModelRegistrationRecord.key` |
| `label` | `String` | | |
| `class_path` | `String` | **migration-only**; never used for dispatch (GC-eos-state-04) | `ModelRegistrationRecord.class_path` |
| `kwargs_json` | `String` | **opaque**; parsing/redaction/env-resolution owned by `eos-db` (out of scope here) | `ModelRegistrationRecord.kwargs_json` |
| `is_active` | `bool` | exactly one active | |
| `created_at`/`updated_at` | `UtcDateTime` | | |

`llm_provider` is **not** stored as a column today; it is derived during the
`eos-db` migration from `class_path`/`kwargs`. `eos-state` does **not** model
provider selection at all: `ModelStore` returns the normalized
`ModelRegistration` (carrying `model_key`), and *provider dispatch* is the job of
`eos-db`/`eos-llm-client` — keeping this crate free of both `class_path`
interpretation and any `eos-llm-client` dependency (preserves the DAG).

### 6.8 `ExecutionTaskOutcome` + projections (source: `outcomes.py`)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcomeStatus { Success, Failed }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRole { Generator, Reducer }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionTaskOutcome {
    pub status: TaskOutcomeStatus,
    pub role: ExecutionRole,
    pub task_id: TaskId,
    pub outcome: String,
}
```

`TaskOutcomeStatus`/`ExecutionRole` replace Python `Literal[...]` aliases with
exhaustive enums (`type-no-stringly`, `type-enum-states`). Wire values match the
Python strings (`"success"`/`"failed"`, `"generator"`/`"reducer"`).

Pure projection functions (parameters borrow; `own-borrow-over-clone`):

| Rust fn | Python source | Shape |
|---|---|---|
| `latest_iteration(iterations: &[Iteration]) -> Option<&Iteration>` | `workflow_outcomes` (selection half) | sync; argmax by `sequence_no` (the latest iteration) |
| `present_status(raw: &str) -> TaskOutcomeStatus` | `present_status` | sync; `"done" → Success` else `Failed` |
| `execution_outcome_for_submission(task_id, role, status, outcome) -> ExecutionTaskOutcome` | `execution_outcome_for_submission` | sync constructor |
| `async project_attempt_outcomes(attempt: &Attempt, store: Option<&dyn TaskStore>) -> Result<Vec<ExecutionTaskOutcome>, CoreError>` | `project_attempt_outcomes` | `None` store ⇒ return `attempt.outcomes.clone()` |
| `async attempt_execution_outcomes(attempt, store) -> Result<Vec<…>, CoreError>` | `attempt_execution_outcomes` | persisted-or-recompute |
| `async project_iteration_outcomes(attempts: &[Attempt], store) -> Result<Vec<…>, CoreError>` | `project_iteration_outcomes` | closing-attempt-only filter (see §8) |

**Only the `json.loads` half of `workflow_outcomes` moves to `eos-db`; the
selection stays here** (resolves the serde_json seam, GC-eos-state-03). Python
`workflow_outcomes` does two things: (1) pick the iteration with the max
`sequence_no`, and (2) **`json.loads` that iteration's serialized `outcomes`
string**. Step (1) is a pure domain projection and is **kept in `eos-state`** as
`latest_iteration` (anchor §5 assigns "outcome projections" here — ceding it would
narrow this crate's ownership). Step (2) requires `serde_json`, which §2 forbids
here, so the string-parse alone moves to the DB boundary: since
`Iteration.outcomes: Option<String>` is a *serialized* projection owned by `eos-db`
(which already owns the string⇆records codec), `eos-workflow` composes
`eos-state::latest_iteration` with the `eos-db` parse of that iteration's
`outcomes`. `eos-state`'s other projections (`project_attempt_outcomes`,
`attempt_execution_outcomes`, `project_iteration_outcomes`) never touch a JSON
string — they read typed `attempt.outcomes`/`task.outcomes` and filter.

The Python JSON helpers (`to_record`, `parse_outcomes_record`, `records_json`,
`task_outcomes_from_row`, `execution_outcomes_from_row`, `_outcomes_from_record`)
operate on raw dict/str rows. Their **typed core** stays here as pure fns: the
closing-attempt filter over `ExecutionTaskOutcome`, and `latest_iteration` over
`&[Iteration]`; the **string ⇆ records** boundary (`json.loads`/`json.dumps`)
moves to `eos-db`.
Crucially, the **fallback normalization** that `task_outcomes_from_row` /
`_outcomes_from_record` perform — missing `status` → `present_status(task.status)`,
missing `role` → `_execution_role(task.role)` (the owning task's role), and the
`(no outcome recorded)` default for an empty `outcome` — is applied at the
`eos-db` write/parse boundary, so the typed `Task.outcomes: Vec<ExecutionTaskOutcome>`
that `eos-state` reads is already complete and pre-normalized. **Two distinct
status normalizers must both be reproduced at that `eos-db` boundary** (they are
not the same mapping): `_normalize_status` normalizes the **per-record** `status`
field (`"success" → Success`, everything else → `Failed`, so `"done"` →
`Failed`); `present_status` only *fills a missing* per-record `status` from the
owning task's status (`"done" → Success` else `Failed`). The composition is
consistent because `present_status` emits only `success`/`failed`, which
`_normalize_status` passes through. The implementer must **not** apply
`present_status` uniformly to a present record `status` — that would wrongly map
`"done" → Success`. `project_attempt_outcomes`
therefore never sees an un-normalized record and matches Python without re-deriving
fallbacks (proven by AC-eos-state-09). This is the single behavioral seam shift
from the Python module and is recorded as GC-eos-state-03.

### 6.9 Store trait method roster (mirrors Python protocols + promoted stores)

| Trait | Methods (async, `-> Result<_, CoreError>`) | Python source |
|---|---|---|
| `WorkflowStore` | `insert`, `get`, `append_iteration_id`, `set_status`, `list_for_parent_task` | `WorkflowStoreProtocol` |
| `IterationStore` | `insert`, `get`, `append_attempt_id`, `set_status`, `set_deferred_goal_for_next_iteration`, `close_succeeded`, `list_for_workflow` | `IterationStoreProtocol` |
| `AttemptStore` | `insert`, `get`, `set_stage`, `set_planner_task_id`, `set_generator_task_ids`, `set_reducer_task_ids`, `set_deferred_goal`, `close`, `list_for_iteration` | `AttemptStoreProtocol` |
| `TaskStore` | `upsert_task`, `get`, `set_task_status`, `set_task_status_if_current` | `TaskStoreProtocol` (task surface) |
| `RequestStore` | `create_request`, `get`, `set_root_task_id`, `finish_request` | `TaskStoreProtocol` (request surface, split out per ISP) |
| `AgentRunStore` | `create_run`, `finish_run`, `get` | `AgentRunStore` |
| `ModelStore` | `register`, `delete`, `get`, `active` | `ModelStore` |

`AttemptStore::close` mirrors Python: `status: AttemptStatus`,
`fail_reason: Option<AttemptFailReason>`, `outcomes: Option<&[ExecutionTaskOutcome]>`,
`closed_at: UtcDateTime`. `ModelStore::get`/`active` return
`Result<Option<ModelRegistration>, CoreError>` (the normalized DTO carrying
`model_key`); **provider selection is not modeled in `eos-state`** — it is
`eos-db`/`eos-llm-client`'s job (see §6.7), which keeps this crate free of
`class_path` interpretation and of any `eos-llm-client` dependency.

### 6.10 Terminal submission DTOs (source: `workflow/submissions.py`)

All four are owned here (anchor §5 "terminal submission DTOs"). Each derives
`Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`. `Literal[...]`
fields become enums; `status` **reuses** `TaskOutcomeStatus` (DRY — do not mint a
parallel enum).

**`PlannerSubmission`** — validated planner submission from a full/partial plan tool:

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `attempt_id` | `AttemptId` | | `PlannerSubmission.attempt_id` |
| `planner_task_id` | `TaskId` | | |
| `kind` | `PlannerKind` | new enum `{ Completes, Defers }` from `Literal["completes","defers"]`, snake_case | `PlannerSubmission.kind` |
| `generator_task_ids` | `Vec<TaskId>` | | |
| `reducer_task_ids` | `Vec<TaskId>` | | |
| `deferred_goal_for_next_iteration` | `Option<String>` | normalized name (anchor §4) | |

**`PlannerFailureSubmission`** — runtime-synthesized planner failure:

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `attempt_id` | `AttemptId` | | `PlannerFailureSubmission.attempt_id` |
| `planner_task_id` | `TaskId` | | |
| `fail_reason` | `PlannerFailReason` | **distinct** one-variant enum `{ RunExhausted }` from `Literal["run_exhausted"]` | `PlannerFailureSubmission.fail_reason` |

> **Do not conflate `PlannerFailReason` with `AttemptFailReason`** (§6.4). They
> are separate reason vocabularies: `PlannerFailReason::RunExhausted` is a
> planner-stage synthesis; `AttemptFailReason::{TaskFailed, StartupFailed}` is the
> attempt-close reason. `eos-db`/`eos-workflow` must keep the two distinct.

**`GeneratorSubmission`** / **`ReducerSubmission`** — identical shape (validated
terminal outcome for one generator / one reducer task; the reducer's is binary):

| Field | Rust type | Notes | Source |
|---|---|---|---|
| `attempt_id` | `AttemptId` | | `*.attempt_id` |
| `task_id` | `TaskId` | | |
| `status` | `TaskOutcomeStatus` | **reuse** §6.8 enum (`success`/`failed`) | `*.status` |
| `outcome` | `String` | | |
| `terminal_tool_result` | `JsonObject` | non-optional (always present on a terminal submit) | `*.terminal_tool_result` |

## 7. Concurrency & State Ownership

`eos-state` is **runtime-agnostic** (anchor §7): it spawns nothing, owns no
runtime, holds no locks, and exposes only `&self` async trait methods. All
concurrency primitives live downstream.

- **Shared immutable data:** every DTO is `Clone` and cheap-ish to clone (owned
  `String`/`Vec`); store impls return fresh owned values, so the "frozen DTO"
  Python invariant is preserved without `Arc` or interior mutability inside this
  crate. Consumers that share a DTO across tasks wrap it in `Arc<T>`
  themselves (`own-arc-shared`) — not this crate's concern.
- **Store traits:** `Send + Sync`, used behind `Arc<dyn WorkflowStore>` etc. in
  `eos-runtime`. Connection concurrency / single-writer serialization is the
  `SqlitePool`'s job in `eos-db` (anchor §7 "no app-level DB mutex"); this crate
  imposes **no lock discipline** because it holds no state across `.await`
  (`async-no-lock-await` is trivially satisfied — there are no locks).
- **Projections:** the async projection fns `.await` exactly one or more
  `store.get`/`list_*` calls and own all their intermediate `Vec`s on the
  stack; nothing is shared, so there is no lock-across-await hazard
  (`anti-lock-across-await`).
- **No channels, no `JoinSet`, no `CancellationToken`** originate here — the
  background supervisor and attempt scheduler (anchor §7) live in `eos-engine` /
  `eos-workflow` and consume these traits.

## 8. Behavior & Invariants

Cite the plan (§2, §3, anchor §3) — these semantics must port exactly:

1. **Task is the persisted agent interface.** A root request mints one
   `Task { role: Root, workflow_id: None }`; delegation creates
   `Workflow → Iteration → Attempt`. No synthetic root workflow (anchor §2). The
   DTO field defaults must allow `Task` with all of `workflow_id`/`iteration_id`/
   `attempt_id` = `None`.
2. **`parent_task_id` is never mutated at workflow close** (anchor §3). `Workflow`
   exposes no setter for it; `set_status` touches only status/closed_at/outcomes.
3. **Reducer is the exit gate.** `Attempt.reducer_task_ids` is the gate set; the
   single RUN stage schedules `generator_task_ids ∪ reducer_task_ids` to
   quiescence (scheduling itself lives in `eos-workflow`).
4. **Iteration-outcome projection (`project_iteration_outcomes`)** — preserve the
   subtle filter exactly: on a **passing** closing attempt, surface only
   `role == Reducer && status == Success`; on a **failed** close, surface only
   `(role ∈ {Generator, Reducer}) && status == Failed`. Reducer successes from
   *earlier failed attempts* are internal history and are **never** surfaced. The
   projection uses only `attempts.last()` (the closing attempt). This is the
   highest-risk behavior to regress — it gets its own AC + property test (AC-eos-state-05).
5. **`attempt_execution_outcomes`**: return persisted `attempt.outcomes` when
   non-empty, else recompute from task rows via `project_attempt_outcomes`.
6. **`workflow_outcomes` semantics** = projection of the iteration with the max
   `sequence_no` (latest), parsed from that iteration's serialized `outcomes`.
   The **latest-by-`sequence_no` selection stays in `eos-state`**
   (`latest_iteration`); only the `json.loads` of that iteration's serialized
   `outcomes` string is **owned by `eos-db`**. `eos-workflow` composes the two
   (§6.8).
7. **`present_status`**: `"done" → Success`, everything else → `Failed` (exact).
8. **Optimistic concurrency**: `TaskStore::set_task_status_if_current` returns
   `Ok(None)` when the stored status ≠ `expected` (no-op), `Ok(Some(task))` on a
   successful flip — preserving the Python `set_task_status_if_current` contract
   that the engine relies on for terminal-tool stamping.
9. **Single active model**: `ModelStore` invariant that exactly one registration
   is `is_active` is enforced in `eos-db` (deactivate-all on activate); this crate
   only models the boolean.
10. **Serialized-projection boundary**: `outcomes: Option<String>` on
    `Workflow`/`Iteration` is a JSON string the **DB** writes/parses;
    `outcomes: Vec<ExecutionTaskOutcome>` on `Task`/`Attempt` is the typed
    in-memory form. The crate never calls `serde_json` itself (GC-eos-state-03).
    On the mutating store methods (`set_status`, `set_task_status`,
    `set_task_status_if_current`, `close`), `None` on the `outcomes` param means
    **leave the persisted projection unchanged**; there is intentionally no
    "clear to empty" path, matching the Python concrete store
    (`if outcomes is not None: record.outcomes = outcomes`).

**No new error enum.** Store methods return `eos-types::CoreError` (anchor §5/§8);
adding a crate-local error here would force every downstream `#[from]` to chain
two enums for no benefit. Pure projection fns that touch a store return
`Result<_, CoreError>` (propagating store errors with `?`, `err-question-mark`);
purely in-memory fns return plain values (`err-result-over-panic` — no panics).

## 9. SOLID & Principles Applied

- **ISP** — the seven per-entity `Store` traits are the headline ISP win: no
  god-store. Each trait carries only the methods agent-core calls (the Python
  "narrow contract" rule). `RequestStore` is split out of the Python combined
  `TaskStoreProtocol` so a task-only consumer needn't see request CRUD.
- **DIP** — `eos-state` defines the abstractions; `eos-db` implements them;
  `eos-runtime` wires concretes behind `Arc<dyn ...>`. High-level workflow/engine
  code depends on these traits, never on `eos-db` (anchor §6 seam map row
  "per-entity `Store` traits").
- **LSP** — exhaustive enums (`TaskStatus`, `AttemptStage`, `ExecutionRole`, …)
  make every state substitutable and force exhaustive `match`; in-memory test
  fakes are substitutable for sqlx repos via the sealed traits (`test-mock-traits`).
- **OCP** — adding a persisted entity = adding a new small trait + DTO, not
  editing an existing god-store. Sealing (`api-sealed-trait`) lets traits gain
  methods without breaking external crates.
- **SRP** — this crate is *only* domain shapes + store contracts + pure
  projections. It explicitly does **not** build SQL, parse JSON strings, redact
  secrets, interpret `class_path`, or own lifecycle (those are `eos-db` /
  `eos-engine` / `eos-workflow`).
- **KISS/YAGNI/DRY** — no builder types for these flat DTOs (plain structs;
  `api-builder-pattern` is for *complex* construction only); no crate-local error
  enum; no `serde_json` dependency; provider/message typing deferred to their
  owning crates rather than duplicated. The only abstractions introduced are the
  store traits already mandated by anchor §6 — nothing speculative.
- **Non-goals respected** (anchor §2): no SQL/Postgres, no `class_path` dispatch
  (migration-only field), no synthetic root workflow, no global orchestrator
  (this crate has no orchestration at all), no peer messaging.

## 10. Gap Closeouts (tracked requirements)

- **GC-eos-state-01** — *Typed task boundary (close the `id`/`task_id`
  double-serialize gap, plan §1 + §2 carry-over).* `TaskStore` returns typed
  `Option<Task>` and accepts `&Task` for `upsert_task`; the Python
  `TaskRow = dict[str, Any]` is dropped. **Resolution:** no dict rows anywhere in
  the trait surface — typed fields at every boundary. Proven by AC-eos-state-06.
- **GC-eos-state-02** — *Normalize generator/executor naming.* State role enum is
  `Generator`; `ExecutionRole` has only `Generator`/`Reducer`; `executor` is
  absent from `eos-state`. **Resolution:** `AgentRole`/`ExecutionRole` variants
  fixed; a grep test asserts no `Executor`/`executor` token in `eos-state` source
  (AC-eos-state-02).
- **GC-eos-state-03** — *Goal-field naming + serialized-projection seam.* Domain
  fields are `workflow_goal`, `iteration_goal`, `deferred_goal_for_next_iteration`
  (anchor §4); DB-column mapping stays in `eos-db`. JSON string (de)serialization
  of the `outcomes` field also lives in `eos-db`; `eos-state` keeps only the typed
  projection algebra. **Resolution:** field names per §6; no `serde_json` dep;
  projections operate on typed `ExecutionTaskOutcome`. Proven by AC-eos-state-01.
  The `eos-db` parse boundary must reproduce **both** Python status normalizers —
  `_normalize_status` on a present per-record `status` (`"done" → Failed`) and
  `present_status` to fill a missing per-record `status` from the task status
  (`"done" → Success`) — not `present_status` uniformly; the fallback-parity AC
  is owned by impl-eos-db.md (AC-eos-state-09 already cedes raw-record parity there).
- **GC-eos-state-04** — *No `class_path` dispatch (anchor §2 non-goal).*
  `ModelRegistration.class_path` is an opaque migration-only field; provider
  selection is normalized to `llm_provider`/`model_key` and is **not modeled in
  `eos-state`** — it is derived in `eos-db`. `ModelStore::get`/`active`
  return the `ModelRegistration` DTO (carrying `model_key`) only. **Resolution:**
  no `class_path`-based branching and no provider-selection logic in `eos-state`;
  the DTO carries the column but no behavior. Proven by AC-eos-state-07.
- **GC-eos-state-05** — *Acyclic role ownership.* `Task.role` uses a minimal
  4-variant `TaskRole` defined here (upstream of `eos-agent-def`); the full
  profile role/type registry keeps the `AgentRole` name in `eos-agent-def` and
  maps to/from `TaskRole` at spawn/validation boundaries.
  **Resolution:** documented ownership boundary in §6.1; verified by the
  dependency graph in overview.md having no `eos-state → eos-agent-def` edge.

## 11. Acceptance Criteria

TDD: write each test first, confirm it fails for the right reason, then implement.
Maps to anchor §11 "Tests to Port First" → `eos-db` row (store roundtrips) and
the Phase-0 schema-snapshot harness.

- **AC-eos-state-01** — `project_iteration_outcomes` / `attempt_execution_outcomes`
  reproduce the Python projection exactly. *Test:* `outcomes_projection_parity`
  (`#[tokio::test]`) using an in-crate `FakeTaskStore` seeded to mirror the Python
  `test_*` fixtures; asserts the passing-attempt reducer-success filter and
  failed-attempt failed-task filter. (Ports the outcomes behavior referenced by
  the eos-workflow/eos-db port lists.)
- **AC-eos-state-02** — No `executor`/`Executor` token appears in `eos-state`
  source and `AgentRole`/`ExecutionRole` variants match §6. *Test:*
  `state_role_naming_is_generator` (compile-time variant assertion + a source-grep
  unit test). (GC-eos-state-02.)
- **AC-eos-state-03** — Every DTO/enum round-trips through serde with the exact
  Python wire strings (`snake_case`, `deferred_goal_continuation`, `success`,
  `generator`, …). *Test:* `serde_wire_values_match_python` table test +
  `proptest` round-trip (`serialize→deserialize == identity`).
- **AC-eos-state-04** — `schemars` JSON Schema for the four submission DTOs and
  `ExecutionTaskOutcome` matches the Phase-0 snapshot of the current Pydantic/
  dataclass schema. *Test:* `submission_schema_snapshot` (anchor §11 parity
  harness; `insta`/golden snapshot).
- **AC-eos-state-05** — Reducer successes from *earlier failed attempts* are never
  surfaced in iteration outcomes. *Test:* `earlier_attempt_reducer_success_hidden`
  — two attempts, first failed (with a successful reducer), second passing;
  asserts only the closing attempt's reducer successes appear. (§8 invariant 4,
  highest-risk regression.)
- **AC-eos-state-06** — `TaskStore` is fully typed: `get → Option<Task>`,
  `upsert_task(&Task)`, no `dict`/`Value` row type in any signature. *Test:*
  `task_store_is_typed` (a `FakeTaskStore` impl + a trait-object usage compiles
  and round-trips a `Task` losslessly). (GC-eos-state-01.)
- **AC-eos-state-07** — `ModelRegistration` exposes `model_key` (not `key`),
  treats `class_path` as opaque, and `eos-state` contains no `class_path`-based
  branch. *Test:* `model_registration_no_class_path_dispatch` (field-presence +
  source-grep). (GC-eos-state-04.)
- **AC-eos-state-08** — `set_task_status_if_current` returns `Ok(None)` on status
  mismatch and `Ok(Some(_))` on a successful flip. *Test:*
  `optimistic_status_flip` against `FakeTaskStore`. (§8 invariant 8.)
- **AC-eos-state-09** — `project_attempt_outcomes` matches Python over an
  already-normalized `Task.outcomes`. Because `ExecutionTaskOutcome.status`/`role`
  are non-`Option` enums, a record missing those fields is unrepresentable in
  `eos-state` — the `present_status(task.status)` / `_execution_role(task.role)` /
  `(no outcome recorded)` fallbacks are applied at the `eos-db` parse boundary, so
  `eos-state` only ever projects complete records. *Test:*
  `project_attempt_outcomes_pre_normalized` (FakeTaskStore with a status/role-complete
  `Task.outcomes`). The fallback-normalization parity against Python
  `task_outcomes_from_row` is owned by impl-eos-db.md (it is the only crate that
  sees raw records). (§6.8 normalization-boundary seam.)

All store-trait behavior ACs use in-crate `#[cfg(test)] mod tests` fakes
(`test-cfg-test-module`, `test-use-super`, `test-mock-traits`); cross-crate
roundtrips against the real sqlx repos are owned by impl-eos-db.md.

## 12. Implementation Checklist

1. Add the crate to the workspace; inherit `serde`, `schemars`, `async-trait`
   from `[workspace.dependencies]`; depend on `eos-types` (`proj-workspace-deps`).
2. Port the lifecycle enums (`TaskStatus`, `WorkflowStatus`, `IterationStatus`,
   `IterationCreationReason`, `AttemptStage`, `AttemptStatus`, `AttemptFailReason`,
   `AgentRole`, `ExecutionRole`, `TaskOutcomeStatus`) with serde wire values +
   AC-eos-state-03 round-trip test first.
3. Port the DTOs (`task.rs`, `workflow.rs`, `iteration.rs`, `attempt.rs`,
   `request.rs`, `agent_run.rs`, `model.rs`) with their `is_open`/`is_closed`/
   `attempt_count`/… methods.
4. Port `ExecutionTaskOutcome` + submission DTOs; add AC-eos-state-04 schema
   snapshot.
5. Write `FakeTaskStore`/`FakeIterationStore` test fakes, then the projection
   parity tests (AC-eos-state-01, -05) **before** porting `outcomes.rs` fns.
6. Implement the pure projection fns over the fakes until the tests pass.
7. Declare the seven `#[async_trait]` sealed `Store` traits in `store.rs`; add
   AC-eos-state-06/-07/-08 against fakes.
8. Re-export the public surface from `lib.rs`; run `cargo fmt --check` +
   `clippy -D warnings` (`lint-rustfmt-check`).

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-state` per spec-conventions.md §13. Do not edit other crates' rows.

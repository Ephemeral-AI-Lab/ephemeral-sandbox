# Crate `eos-workflow` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-workflow/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**34 types across 15 files.**

The `eos-workflow` crate owns delegated workflow lifecycle, per-attempt orchestration, run-stage scheduling, launch context composition, and the workflow-context packets. It depends on store traits (`eos-state`) and downstream-state ports (`eos-tools`), not concrete persistence or engine crates, so it owns only delegated workflow state while root requests remain direct root tasks. The central types form a lifecycle spine — `WorkflowStarter` mints state from a running task, `WorkflowLifecycle` drives workflow/iteration creation and close, `IterationAttemptCoordinator` (held in `OpenIterationCoordinatorRegistry`) governs attempts, and `AttemptOrchestrator` (held in `AttemptOrchestratorRegistry`) runs one Attempt's PLAN → RUN → CLOSED machine with `AttemptStageAdvancer` as the single-writer RUN scheduler. Agent execution is abstracted behind the `AgentRunner` trait fed `AgentLaunch` descriptors built by `AgentLaunchFactory`/`AgentEntryComposer`; `ContextEngine` renders role-scoped `AgentContext` packets from `ContextScope`. `PlanSubmissionAdapter` and `WorkflowControlAdapter` are the inbound `eos-tools` port adapters consumed by the runtime/engine, and `WorkflowError`/`Result` form the crate error boundary.

## Contents

- **`eos-workflow/src/error.rs`** — `Result`, `WorkflowError`
- **`eos-workflow/src/ids.rs`** — `WorkflowLifecycleConfig`
- **`eos-workflow/src/lifecycle.rs`** — `WorkflowLifecycle`
- **`eos-workflow/src/starter.rs`** — `StartedWorkflow`, `WorkflowStarter`
- **`eos-workflow/src/ports.rs`** — `PlanSubmissionAdapter`, `WorkflowControlAdapter`, `WorkflowHandleRegistry`, `WorkflowHandleMaps`
- **`eos-workflow/src/attempt/launch.rs`** — `AgentTerminal`, `AgentRunReport`, `AgentRunner`, `AgentLaunch`, `AttemptDeps`, `AgentLaunchFactory`, `LaunchBuildArgs`
- **`eos-workflow/src/attempt/orchestrator.rs`** — `ExecutionMark`, `AttemptOrchestrator`
- **`eos-workflow/src/attempt/orchestrator_registry.rs`** — `AttemptOrchestratorRegistry`
- **`eos-workflow/src/attempt/plan_dag.rs`** — `DagStatus`
- **`eos-workflow/src/attempt/run_stage.rs`** — `AttemptStageAdvancer`
- **`eos-workflow/src/iteration/mod.rs`** — `IterationClosed`, `IterationClosedCallback`, `IterationAttemptCoordinator`, `OpenIterationCoordinatorRegistry`
- **`eos-workflow/src/context/scope.rs`** — `ContextScope`
- **`eos-workflow/src/context/section.rs`** — `ContextRole`, `ContextSection`, `AgentContext`
- **`eos-workflow/src/context/engine.rs`** — `ContextEngineDeps`, `ContextEngine`
- **`eos-workflow/src/context/composer.rs`** — `AgentEntryMessages`, `AgentEntryComposer`

---

## `eos-workflow/src/error.rs`

#### `Result<T>`  ·  _type alias_  ·  = `std::result::Result<T, WorkflowError>`  ·  [L4]

Result alias for workflow operations.

#### `WorkflowError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L9]

Workflow lifecycle and context-builder invariant failures.

**Variants**:
- `BlankPrompt` — a delegated workflow prompt was empty after trimming.
- `NotFound { entity: &'static str, id: String }` — a required entity was not found in the store.
- `Invariant(String)` — a lifecycle invariant was violated.
- `Recipe(String)` — context recipe and scope do not line up.
- `MissingContextField(&'static str)` — a context scope omitted a field required by the selected role.
- `AgentDefinition(String)` — an agent definition was missing or invalid for launch.
- `Store(CoreError)` — `#[from]` store failure propagated from an upstream store trait.
- `Json(serde_json::Error)` — `#[from]` JSON encode/decode failure at the outcomes boundary.
- `Tool(eos_tools::ToolError)` — `#[from]` tool-framework fault while adapting a downstream-state port.
- `Join(String)` — a spawned agent task panicked or was cancelled.

**Trait impls**: `Error`, `Display` (via thiserror), `From<CoreError>`, `From<serde_json::Error>`, `From<eos_tools::ToolError>`, `From<eos_agent_def::AgentDefError>`

<details><summary>Methods (2)</summary>

`invariant`, `not_found`

</details>

---

## `eos-workflow/src/ids.rs`

#### `WorkflowLifecycleConfig`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L7]

Per-workflow lifecycle knobs injected by `eos-runtime`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `default_attempt_budget` | `i64` | `pub` |

**Trait impls**: `Default`

---

## `eos-workflow/src/lifecycle.rs`

#### `WorkflowLifecycle`  ·  _struct_  ·  derives: `Clone`  ·  [L27]

Workflow-level lifecycle coordinator (workflow + iteration creation and close).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `deps` | `AttemptDeps` |  |
| `iteration_coordinators` | `Arc<OpenIterationCoordinatorRegistry>` |  |
| `config` | `WorkflowLifecycleConfig` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (7)</summary>

`new`, `create_workflow`, `create_iteration_with_coordinator`, `create_iteration_with_coordinator_inner`, `handle_iteration_closed`, `close_workflow`, `require_workflow`

</details>

---

## `eos-workflow/src/starter.rs`

#### `StartedWorkflow`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L9]

Workflow start result (ids minted by a successful `WorkflowStarter::start`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `parent_task_id` | `TaskId` | `pub` |
| `parent_attempt_id` | `Option<AttemptId>` | `pub` |
| `workflow_id` | `eos_state::WorkflowId` | `pub` |
| `iteration_id` | `eos_state::IterationId` | `pub` |
| `attempt_id` | `AttemptId` | `pub` |

#### `WorkflowStarter`  ·  _struct_  ·  derives: `Clone`  ·  [L24]

Single safe entry point from a running task to a delegated workflow.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `deps` | `AttemptDeps` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (4)</summary>

`new`, `start`, `assert_parent_running_and_no_open_child`, `compensate_failed_start`

</details>

---

## `eos-workflow/src/ports.rs`

#### `PlanSubmissionAdapter`  ·  _struct_  ·  derives: `Clone`  ·  [L26]

Adapter from `eos-tools` planner/generator/reducer terminal ports to active per-attempt orchestrators.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `registry` | `Arc<AttemptOrchestratorRegistry>` |  |

**Trait impls**: `Debug` (manual), `eos_tools::ports::Sealed`, `PlanSubmissionPort` (async)

<details><summary>Methods (1)</summary>

`new`

</details>

#### `WorkflowControlAdapter`  ·  _struct_  ·  derives: `Clone`  ·  [L98]

Adapter from `eos-tools` workflow-control ports to delegated workflow state.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `starter` | `WorkflowStarter` |  |
| `workflow_store` | `Arc<dyn eos_state::WorkflowStore>` |  |
| `iteration_store` | `Arc<dyn eos_state::IterationStore>` |  |
| `attempt_store` | `Arc<dyn eos_state::AttemptStore>` |  |
| `task_store` | `Arc<dyn TaskStore>` |  |
| `handles` | `Arc<WorkflowHandleRegistry>` |  |

**Trait impls**: `Debug` (manual), `eos_tools::ports::Sealed`, `WorkflowControlPort` (async)

<details><summary>Methods (3)</summary>

`new`, `cancel_workflow_state`, `cancel_active_task`

</details>

#### `WorkflowHandleRegistry`  ·  _struct_  ·  derives: `Debug, Default`  ·  private  ·  [L335]

Process-local mint/lookup of `wf_<n>` workflow session handles.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `next_handle` | `AtomicU64` |  |
| `inner` | `Mutex<WorkflowHandleMaps>` |  |

<details><summary>Methods (2)</summary>

`handle_for_workflow`, `workflow_id_for_handle`

</details>

#### `WorkflowHandleMaps`  ·  _struct_  ·  derives: `Debug, Default`  ·  private  ·  [L341]

Bidirectional handle/workflow-id maps guarded by the registry mutex.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_by_handle` | `HashMap<WorkflowSessionId, WorkflowId>` |  |
| `handle_by_workflow` | `HashMap<WorkflowId, WorkflowSessionId>` |  |

---

## `eos-workflow/src/attempt/launch.rs`

#### `AgentTerminal`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq`  ·  [L21]

Terminal result returned by an agent run.

**Variants**: `Planner(PlannerPlan)`, `PlannerFailure(PlannerFailureSubmission)`, `Generator(GeneratorSubmission)`, `Reducer(ReducerSubmission)`

#### `AgentRunReport`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L34]

Result of one agent run at the workflow seam.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `terminal` | `Option<AgentTerminal>` | `pub` |
| `failure_summary` | `Option<String>` | `pub` |

<details><summary>Methods (2)</summary>

`terminal`, `no_terminal`

</details>

#### `AgentRunner`  ·  _trait_  ·  bases: `Send + Sync`  ·  async  ·  [L63]

Runtime adapter seam over the engine's agent runner.

**Trait items**:
- `async fn run(&self, launch: AgentLaunch) -> Result<AgentRunReport>;`

#### `AgentLaunch`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L70]

Launch descriptor for one workflow agent.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `task_id` | `TaskId` | `pub` |
| `request_id` | `RequestId` | `pub` |
| `attempt_id` | `Option<eos_state::AttemptId>` | `pub` |
| `role` | `AgentRole` | `pub` |
| `agent_name` | `String` | `pub` |
| `context` | `String` | `pub` |
| `task_guidance` | `Option<String>` | `pub` |
| `needs` | `Vec<TaskId>` | `pub` |
| `agent_def` | `Option<AgentDefinition>` | `pub` |
| `workflow_id` | `Option<WorkflowId>` | `pub` |
| `skill` | `Option<String>` | `pub` |

#### `AttemptDeps`  ·  _struct_  ·  derives: `Clone`  ·  [L97]

Per-attempt dependency bundle (stores, registries, runner seam, lifecycle knobs).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_store` | `Arc<dyn WorkflowStore>` | `pub` |
| `iteration_store` | `Arc<dyn IterationStore>` | `pub` |
| `attempt_store` | `Arc<dyn AttemptStore>` | `pub` |
| `task_store` | `Arc<dyn TaskStore>` | `pub` |
| `agent_registry` | `Arc<AgentRegistry>` | `pub` |
| `orchestrator_registry` | `Arc<AttemptOrchestratorRegistry>` | `pub` |
| `iteration_coordinators` | `Option<Arc<OpenIterationCoordinatorRegistry>>` | `pub` |
| `lifecycle_config` | `WorkflowLifecycleConfig` | `pub` |
| `composer` | `Option<Arc<AgentEntryComposer>>` | `pub` |
| `audit_sink` | `Arc<dyn AuditSink>` | `pub` |
| `runner` | `Arc<dyn AgentRunner>` | `pub` |
| `max_concurrent_task_runs` | `usize` | `pub` |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (2)</summary>

`new`, `request_id_for_attempt`

</details>

#### `AgentLaunchFactory`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L181]

Role-parametrized launch factory (builds planner/generator/reducer launches).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `deps` | `AttemptDeps` |  |

<details><summary>Methods (5)</summary>

`new`, `for_planner`, `for_generator`, `for_reducer`, `build`

</details>

#### `LaunchBuildArgs<'a>`  ·  _struct_  ·  private  ·  [L185]

Internal argument bundle for `AgentLaunchFactory::build`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base_agent_name` | `&'a str` |  |
| `role` | `AgentRole` |  |
| `scope` | `ContextScope` |  |
| `task_id` | `TaskId` |  |
| `request_id` | `RequestId` |  |
| `attempt_id` | `Option<eos_state::AttemptId>` |  |
| `needs` | `Vec<TaskId>` |  |
| `workflow_id` | `Option<WorkflowId>` |  |

---

## `eos-workflow/src/attempt/orchestrator.rs`

#### `ExecutionMark`  ·  _struct_  ·  private  ·  [L21]

Internal record describing how to mark one generator/reducer execution task terminal.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `task_id` | `eos_state::TaskId` |  |
| `expected_role` | `TaskRole` |  |
| `outcome_role` | `ExecutionRole` |  |
| `status` | `TaskOutcomeStatus` |  |
| `outcome` | `String` |  |
| `terminal_tool_result` | `eos_state::JsonObject` |  |

#### `AttemptOrchestrator`  ·  _struct_  ·  [L31]

State machine for one Attempt (PLAN/RUN/CLOSED, plan materialization, submission application, close).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `attempt_id` | `AttemptId` |  |
| `deps` | `AttemptDeps` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (24)</summary>

`new`, `attempt_id`, `start`, `spawn_planner_run`, `apply_planner_report`, `synthesize_planner_failure`, `apply_plan`, `materialize_plan_tasks`, `validate_plan_shape`, `apply_plan_submission`, `apply_planner_failure`, `apply_generator_submission`, `apply_reducer_submission`, `record_generator_submission`, `record_reducer_submission`, `mark_execution_task`, `close_attempt`, `plan_task_records`, `fresh_attempt`, `assert_stage`, `validate_planner_submission`, `assert_submission_attempt`, `deps`, `validate_run_concurrency`

</details>

---

## `eos-workflow/src/attempt/orchestrator_registry.rs`

#### `AttemptOrchestratorRegistry`  ·  _struct_  ·  derives: `Default`  ·  [L13]

Process-local liveness map for active attempt orchestrators.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `by_attempt_id` | `Mutex<HashMap<AttemptId, Arc<AttemptOrchestrator>>>` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (4)</summary>

`new`, `register`, `get`, `deregister`

</details>

---

## `eos-workflow/src/attempt/plan_dag.rs`

#### `DagStatus`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L9]

Single-pass DAG status summary over an attempt's persisted plan tasks.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `all_quiescent` | `bool` | `pub` |
| `all_done` | `bool` | `pub` |
| `any_failed_or_blocked` | `bool` | `pub` |

---

## `eos-workflow/src/attempt/run_stage.rs`

#### `AttemptStageAdvancer`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L21]

Single-writer RUN-stage scheduler for one Attempt (fan-out under the concurrency cap, join, close on quiescence).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `orchestrator` | `Arc<AttemptOrchestrator>` |  |
| `cancel` | `CancellationToken` |  |

<details><summary>Methods (7)</summary>

`new`, `advance_run_stage`, `build_launch`, `mark_launch_failed`, `apply_report`, `apply_terminal`, `synthesize_failure`

</details>

---

## `eos-workflow/src/iteration/mod.rs`

#### `IterationClosed`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L16]

Iteration close signal handed to the lifecycle close callback.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `iteration_id` | `IterationId` | `pub` |
| `succeeded` | `bool` | `pub` |
| `deferred_goal` | `Option<String>` | `pub` |

#### `IterationClosedCallback`  ·  _type alias_  ·  = `Arc<dyn Fn(IterationClosed) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>`  ·  [L26]

Async callback invoked when an iteration closes.

#### `IterationAttemptCoordinator`  ·  _struct_  ·  [L30]

Coordinates attempts for one open iteration (create/start/retry, close-on-pass, deferred-goal handoff).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `iteration_id` | `IterationId` |  |
| `deps` | `AttemptDeps` |  |
| `on_iteration_closed` | `IterationClosedCallback` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (12)</summary>

`new`, `iteration_id`, `create_attempt`, `create_and_start_first_attempt`, `start_attempt`, `handle_attempt_closed`, `current_iteration_snapshot`, `close_iteration_passed`, `retry_or_close_failed`, `close_iteration_failed`, `latest_failed_attempt_after`, `close_attempt_after_startup_failure`

</details>

#### `OpenIterationCoordinatorRegistry`  ·  _struct_  ·  derives: `Default`  ·  [L312]

Process-local one-coordinator-per-open-iteration registry.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `by_iteration_id` | `Mutex<HashMap<IterationId, Arc<IterationAttemptCoordinator>>>` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (4)</summary>

`new`, `register`, `get`, `deregister`

</details>

---

## `eos-workflow/src/context/scope.rs`

#### `ContextScope`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L9]

Identity fields a context builder can read (role plus optional workflow/iteration/attempt/task ids).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `role` | `ContextRole` | `pub` |
| `workflow_id` | `Option<WorkflowId>` | `pub` |
| `iteration_id` | `Option<IterationId>` | `pub` |
| `attempt_id` | `Option<AttemptId>` | `pub` |
| `task_id` | `Option<TaskId>` | `pub` |

<details><summary>Methods (7)</summary>

`for_planner`, `for_generator`, `for_reducer`, `workflow_id`, `iteration_id`, `attempt_id`, `task_id`

</details>

---

## `eos-workflow/src/context/section.rs`

#### `ContextRole`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  [L7]

Role-specific context packet kind.

**Variants**: `Planner`, `Generator`, `Reducer`

<details><summary>Methods (1)</summary>

`as_str`

</details>

#### `ContextSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L30]

One XML-like context section (tag, ordered attrs, optional text, child sections).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tag` | `String` | `pub` |
| `attrs` | `Vec<(String, String)>` | `pub` · `#[serde(default)]` |
| `text` | `Option<String>` | `pub` · `#[serde(default)]` |
| `children` | `Vec<ContextSection>` | `pub` · `#[serde(default)]` |

<details><summary>Methods (4)</summary>

`new`, `with_attrs`, `with_text`, `with_children`

</details>

#### `AgentContext`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L80]

Full role context packet (role, sections, directive, explicit context limits).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `role` | `ContextRole` | `pub` |
| `sections` | `Vec<ContextSection>` | `pub` |
| `directive` | `String` | `pub` |
| `context_limits` | `Vec<String>` | `pub` · `#[serde(default)]` |

---

## `eos-workflow/src/context/engine.rs`

#### `ContextEngineDeps`  ·  _struct_  ·  derives: `Clone`  ·  [L15]

Store bundle consumed by context builders.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_store` | `Arc<dyn WorkflowStore>` | `pub` |
| `iteration_store` | `Arc<dyn IterationStore>` | `pub` |
| `attempt_store` | `Arc<dyn AttemptStore>` | `pub` |
| `task_store` | `Arc<dyn TaskStore>` | `pub` |

**Trait impls**: `Debug` (manual)

#### `ContextEngine`  ·  _struct_  ·  derives: `Clone`  ·  [L34]

Role-scoped context packet builder (planner/generator/reducer).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `deps` | `ContextEngineDeps` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (7)</summary>

`new`, `build`, `build_planner_context`, `build_execution_context`, `prior_iteration_sections`, `previous_attempt_sections`, `dependency_sections`

</details>

---

## `eos-workflow/src/context/composer.rs`

#### `AgentEntryMessages`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L14]

Composed launch messages for one agent run (agent def plus rendered context/guidance/skill rows).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent_def` | `AgentDefinition` | `pub` |
| `context` | `String` | `pub` |
| `task_guidance` | `Option<String>` | `pub` |
| `skill` | `Option<String>` | `pub` |

#### `AgentEntryComposer`  ·  _struct_  ·  derives: `Clone`  ·  [L27]

Agent-entry message composer (resolves recipe, renders context XML + task guidance + skill).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `engine` | `ContextEngine` |  |
| `agents` | `Arc<AgentRegistry>` |  |

**Trait impls**: `Debug` (manual)

<details><summary>Methods (2)</summary>

`new`, `compose`

</details>

# Module `task_center` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/task_center/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**78 classes across 32 files.**

The `task_center` module is the persisted multi-agent control plane that owns the durable Workflow -> Iteration -> Attempt hierarchy, where each Attempt runs one planner -> generator-DAG -> evaluator try. Its largest class group is the frozen domain DTOs and `StrEnum` status/stage vocabularies in `workflow/state.py`, `iteration/state.py`, `attempt/state.py`, and `_core/task_state.py` (Workflow/Iteration/Attempt views, origins, closure reports/outcomes, task roles and statuses), alongside the validated terminal-outcome submission DTOs in `submissions.py` that form the tools-to-TaskCenter contract (planner/generator/evaluator submissions). A second group is the ContextEngine subsystem (`context_engine/`), which routes recipe ids against a `ContextScope` to build immutable, priority-tagged `ContextPacket`/`ContextBlock` outputs for role, retry, deferral, and evaluation contexts. The remaining classes are lifecycle and orchestration machinery — per-Attempt orchestration and stage advancement (`attempt/`), workflow start/closure routing (`workflow/`), run bootstrap and sandbox provisioning (`entry/`), and agent-launch composition (`agent_launch/`).

## Contents

- **`task_center/_core/audit.py`** — `TaskCenterAuditEmitter`
- **`task_center/_core/generator_summaries.py`** — `TaskOutcome`
- **`task_center/_core/persistence.py`** — `WorkflowStoreProtocol`, `IterationStoreProtocol`, `AttemptStoreProtocol`, `TaskStoreProtocol`
- **`task_center/_core/primitives.py`** — `TaskCenterInvariantViolation`, `TaskCenterLifecycleConfig`
- **`task_center/_core/task_state.py`** — `TaskCenterTaskRole`, `SpawnReason`, `TaskCenterTaskStatus`
- **`task_center/_core/terminal_tool_routing.py`** — `TerminalRoutingContext`, `TerminalToolSelection`, `TerminalToolRouter`
- **`task_center/agent_launch/composer.py`** — `AgentEntryComposer`
- **`task_center/agent_launch/entry_messages.py`** — `AgentEntryMessages`
- **`task_center/attempt/deps.py`** — `AgentLaunch`, `AttemptDeps`, `AttemptDelegatedWorkflowParentTask`
- **`task_center/attempt/generator_dag.py`** — `GeneratorDagSummary`
- **`task_center/attempt/launch.py`** — `EphemeralAttemptAgentLauncher`, `AgentLaunchFactory`
- **`task_center/attempt/orchestrator.py`** — `AttemptOrchestrator`
- **`task_center/attempt/orchestrator_registry.py`** — `RegisteredAttemptOrchestrator`, `AttemptOrchestratorRegistry`
- **`task_center/attempt/stage_advancer.py`** — `AttemptStageAdvancer`
- **`task_center/attempt/state.py`** — `AttemptStage`, `AttemptStatus`, `AttemptFailReason`, `Attempt`
- **`task_center/context_engine/context_outline.py`** — `_OutlineNode`
- **`task_center/context_engine/core.py`** — `ContextPacketStoreProtocol`, `ContextEngineDeps`, `ContextEngine`
- **`task_center/context_engine/exceptions.py`** — `ContextEngineError`, `RecipeScopeError`, `MissingContextRecipeError`, `AgentDefinitionValidationError`
- **`task_center/context_engine/packet.py`** — `ContextPriority`, `ContextBlockKind`, `ContextRefs`, `ContextBlock`, `ContextPacket`
- **`task_center/context_engine/recipes_registry.py`** — `ContextRecipe`, `RecipeRegistry`
- **`task_center/context_engine/renderer.py`** — `XmlPromptRenderer`
- **`task_center/context_engine/scope.py`** — `ContextScope`
- **`task_center/context_engine/tag_dictionary.py`** — `TagDescriptor`
- **`task_center/entry/bootstrap.py`** — `TaskCenterEntryHandle`, `TaskCenterEntry`
- **`task_center/entry/sandbox_provisioning.py`** — `TaskCenterSandboxBinding`, `TaskCenterSandboxProvisioner`
- **`task_center/iteration/attempt_coordinator.py`** — `IterationAttemptCoordinator`, `OpenIterationCoordinatorRegistry`
- **`task_center/iteration/state.py`** — `IterationStatus`, `IterationCreationReason`, `Iteration`, `FailedAttemptEntry`, `TerminalSuccess`, `SuccessDeferred`, `AttemptPlanFailed`, `IterationClosureReport`
- **`task_center/submissions.py`** — `PlannedGeneratorTask`, `PlannerSubmission`, `PlannerFailureSubmission`, `GeneratorSubmission`, `EvaluatorSubmission`
- **`task_center/workflow/closure_report_router.py`** — `WorkflowClosureReportRouter`
- **`task_center/workflow/lifecycle.py`** — `WorkflowLifecycle`
- **`task_center/workflow/starter.py`** — `StartedWorkflow`, `WorkflowStarter`, `_PreparedWorkflowOrigin`
- **`task_center/workflow/state.py`** — `WorkflowOriginKind`, `WorkflowOrigin`, `WorkflowStatus`, `Workflow`, `WorkflowClosureReport`, `WorkflowClosureDeliveryResult`

---

## `task_center/_core/audit.py`

#### `TaskCenterAuditEmitter`  ·  _class_  ·  [L22]

Small write-only facade around a shared audit sink.

**Instance attributes**: `_sink`

<details><summary>Methods (5)</summary>

`__init__`, `publish`, `task_ready`, `task_launched`, `task_failed`

</details>

---

## `task_center/_core/generator_summaries.py`

#### `TaskOutcome`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L50]

One generator/task outcome, the data behind a ``<task>`` element.

**Fields**

| name | type | default |
|------|------|---------|
| `local_id` | `str` |  |
| `status` | `str` |  |
| `summary` | `str \| None` |  |
| `children` | `tuple['TaskOutcome', ...]` | `()` |
| `failure` | `str \| None` | `None` |
| `raw_status` | `str \| None` | `None` |

<details><summary>Methods (1)</summary>

`is_terminal`

</details>

---

## `task_center/_core/persistence.py`

#### `WorkflowStoreProtocol`  ·  _protocol_  ·  bases: `Protocol`  ·  [L41]

Narrow contract for the workflow persistence surface.

**Fields**

| name | type | default |
|------|------|---------|
| `is_ready` | `bool` |  |

<details><summary>Methods (5)</summary>

`insert`, `get`, `append_iteration_id`, `set_status`, `list_for_parent_task`

</details>

#### `IterationStoreProtocol`  ·  _protocol_  ·  bases: `Protocol`  ·  [L71]

Narrow contract for the iteration persistence surface.

**Fields**

| name | type | default |
|------|------|---------|
| `is_ready` | `bool` |  |

<details><summary>Methods (7)</summary>

`insert`, `get`, `append_attempt_id`, `set_status`, `set_deferred_goal_for_next_iteration`, `close_succeeded`, `list_for_workflow`

</details>

#### `AttemptStoreProtocol`  ·  _protocol_  ·  bases: `Protocol`  ·  [L114]

Narrow contract for the attempt persistence surface.

**Fields**

| name | type | default |
|------|------|---------|
| `is_ready` | `bool` |  |

<details><summary>Methods (9)</summary>

`insert`, `get`, `set_stage`, `set_planner_task_id`, `set_generator_task_ids`, `set_evaluator_task_id`, `set_plan_contract`, `close`, `list_for_iteration`

</details>

#### `TaskStoreProtocol`  ·  _protocol_  ·  bases: `Protocol`  ·  [L152]

Narrow contract for the task-center task/run persistence surface.

**Fields**

| name | type | default |
|------|------|---------|
| `is_ready` | `bool` |  |

<details><summary>Methods (10)</summary>

`create_request`, `create_run`, `get_run`, `finish_run`, `upsert_task`, `get_task`, `list_generator_tasks_for_attempt`, `set_task_status`, `set_task_status_if_current`, `set_task_context_packet_id`

</details>

---

## `task_center/_core/primitives.py`

#### `TaskCenterInvariantViolation`  ·  _exception_  ·  bases: `Exception`  ·  [L14]

Raised when a harness lifecycle invariant is violated.

#### `TaskCenterLifecycleConfig`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L40]

Configurable knobs for the workflow/iteration/attempt lifecycle.

**Fields**

| name | type | default |
|------|------|---------|
| `default_attempt_budget` | `int` | `2` |

---

## `task_center/_core/task_state.py`

#### `TaskCenterTaskRole`  ·  _enum_  ·  bases: `StrEnum`  ·  [L13]

Enumeration of the three TaskCenter agent roles: planner, generator, and evaluator.

**Enum members**: `PLANNER = 'planner'`, `GENERATOR = 'generator'`, `EVALUATOR = 'evaluator'`

#### `SpawnReason`  ·  _enum_  ·  bases: `StrEnum`  ·  [L19]

Why a task row was created. Replaces free-form spawn_reason strings.

**Enum members**: `ATTEMPT_PLANNER = 'attempt_planner'`, `ATTEMPT_GENERATOR = 'attempt_generator'`, `ATTEMPT_EVALUATOR = 'attempt_evaluator'`

#### `TaskCenterTaskStatus`  ·  _enum_  ·  bases: `StrEnum`  ·  [L27]

Enumerates the lifecycle states a TaskCenter task can occupy, from pending through terminal outcomes.

**Enum members**: `PENDING = 'pending'`, `RUNNING = 'running'`, `WAITING_WORKFLOW = 'waiting_workflow'`, `DONE = 'done'`, `FAILED = 'failed'`, `BLOCKED = 'blocked'`

---

## `task_center/_core/terminal_tool_routing.py`

#### `TerminalRoutingContext`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L26]

Identity + dependency bundle for launch-time terminal routing.

**Fields**

| name | type | default |
|------|------|---------|
| `scope` | `ContextScope` |  |
| `deps` | `ContextEngineDeps` |  |

#### `TerminalToolSelection`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L68]

Router output: effective agent definition + context recipe.

**Fields**

| name | type | default |
|------|------|---------|
| `agent_def` | `AgentDefinition` |  |
| `context_recipe` | `str` |  |
| `skill_path` | `Path \| None` | `None` |

#### `TerminalToolRouter`  ·  _class_  ·  [L76]

Depth-aware terminal router. Frontmatter remains the source of truth.

<details><summary>Methods (5)</summary>

`resolve`, `_load_definition`, `_require_recipe`, `_effective_definition`, `_allowed_terminals`

</details>

---

## `task_center/agent_launch/composer.py`

#### `AgentEntryComposer`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L37]

Single launch entry point. Frozen so dependencies are explicit.

**Fields**

| name | type | default |
|------|------|---------|
| `router` | `TerminalToolRouter` |  |
| `engine` | `ContextEngine` |  |
| `renderer` | `XmlPromptRenderer` |  |

<details><summary>Methods (2)</summary>

`default`, `compose`

</details>

---

## `task_center/agent_launch/entry_messages.py`

#### `AgentEntryMessages`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L19]

The composer's output: everything the launcher needs.

**Fields**

| name | type | default |
|------|------|---------|
| `agent_def` | `AgentDefinition` |  |
| `context` | `str` |  |
| `task_guidance` | `str \| None` |  |
| `skill` | `str \| None` |  |
| `packet` | `ContextPacket` |  |
| `context_packet_id` | `str \| None` |  |

---

## `task_center/attempt/deps.py`

#### `AgentLaunch`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L40]

Launch descriptor for one harness agent run.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `task_center_run_id` | `str` |  |
| `attempt_id` | `str \| None` |  |
| `role` | `TaskCenterTaskRole` |  |
| `agent_name` | `str` |  |
| `context` | `str` |  |
| `task_guidance` | `str \| None` |  |
| `needs` | `tuple[str, ...]` |  |
| `agent_def` | `AgentDefinition \| None` | `None` |
| `context_packet_id` | `str \| None` | `None` |
| `workflow_id` | `str \| None` | `None` |
| `skill` | `str \| None` | `None` |

#### `AttemptDeps`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L69]

Dependency bundle wiring stores, launcher, orchestrator, and composer needed for Attempt lifecycle operations.

**Fields**

| name | type | default |
|------|------|---------|
| `workflow_store` | `WorkflowStoreProtocol` |  |
| `iteration_store` | `IterationStoreProtocol` |  |
| `attempt_store` | `AttemptStoreProtocol` |  |
| `task_store` | `TaskStoreProtocol` |  |
| `agent_launcher` | `EphemeralAttemptAgentLauncher` |  |
| `orchestrator_registry` | `AttemptOrchestratorRegistry` |  |
| `iteration_coordinators` | `OpenIterationCoordinatorRegistry \| None` | `None` |
| `lifecycle_config` | `TaskCenterLifecycleConfig` | `field(default_factory=TaskCenterLifecycleConfig)` |
| `composer` | `AgentEntryComposer \| None` | `None` |
| `audit_sink` | `AuditSink` | `field(default_factory=NoopAuditSink)` |

<details><summary>Methods (3)</summary>

`run_id_for_attempt`, `require_composer`, `parent_task_for_delegated_workflow`

</details>

#### `AttemptDelegatedWorkflowParentTask`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L120]

Parent generator task waiting on a delegated child workflow.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `attempt_id` | `str` |  |
| `task_store` | `TaskStoreProtocol` |  |
| `orchestrator_lookup` | `Callable[[str], RegisteredAttemptOrchestrator \| None]` |  |

<details><summary>Methods (3)</summary>

`apply_workflow_closure_report`, `mark_waiting_workflow`, `restore_running_after_failed_workflow_start`

</details>

---

## `task_center/attempt/generator_dag.py`

#### `GeneratorDagSummary`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L93]

Summarizes the aggregate completion state of a generator DAG: quiescence, full completion, and failure presence.

**Fields**

| name | type | default |
|------|------|---------|
| `all_quiescent` | `bool` |  |
| `all_done` | `bool` |  |
| `any_failed_or_blocked` | `bool` |  |

---

## `task_center/attempt/launch.py`

#### `EphemeralAttemptAgentLauncher`  ·  _class_  ·  [L44]

Schedules attempt-scoped ephemeral agents and reports run exhaustion.

**Instance attributes**: `_config`, `_deps_provider`, `_sandbox_id`, `_on_event`, `_runner`, `_pending`

<details><summary>Methods (5)</summary>

`__init__`, `launch`, `wait_for_idle`, `_run_launch`, `_report_unfinished_running_task`

</details>

#### `AgentLaunchFactory`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L307]

Build :class:`AgentLaunch` records for each harness role.

**Fields**

| name | type | default |
|------|------|---------|
| `runtime` | `AttemptDeps` |  |

<details><summary>Methods (5)</summary>

`for_planner`, `for_generator`, `for_evaluator`, `_build`, `_require_iteration`

</details>

---

## `task_center/attempt/orchestrator.py`

#### `AttemptOrchestrator`  ·  _class_  ·  [L60]

Runs one planner -> generator DAG -> evaluator harness attempt.

**Instance attributes**: `_attempt`, `_on_attempt_closed`, `_runtime`, `_stage_advancer`

<details><summary>Methods (20)</summary>

`__init__`, `attempt_id`, `start`, `apply_plan_submission`, `apply_planner_failure`, `apply_generator_submission`, `apply_evaluator_submission`, `apply_workflow_closure_report`, `_build_handoff_rollup`, `_validate_planner_submission`, `_persist_plan_contract`, `_persist_generator_tasks`, `_mark_generator`, `_mark_evaluator`, `_write_submission_status`, `_close_attempt`, `_mark_startup_failed`, `_fresh_attempt`, `_assert_stage`, `_assert_submission_attempt`

</details>

---

## `task_center/attempt/orchestrator_registry.py`

#### `RegisteredAttemptOrchestrator`  ·  _protocol_  ·  bases: `Protocol`  ·  [L25]

The slice of :class:`AttemptOrchestrator` observed by collaborators.

<details><summary>Methods (6)</summary>

`attempt_id`, `start`, `apply_workflow_closure_report`, `apply_planner_failure`, `apply_generator_submission`, `apply_evaluator_submission`

</details>

#### `AttemptOrchestratorRegistry`  ·  _class_  ·  [L42]

In-memory lookup by Attempt id.

**Instance attributes**: `_by_attempt_id`

<details><summary>Methods (5)</summary>

`__init__`, `register`, `get`, `get_or_raise`, `deregister`

</details>

---

## `task_center/attempt/stage_advancer.py`

#### `AttemptStageAdvancer`  ·  _class_  ·  [L44]

Advances generator/evaluator stages until the attempt blocks or closes.

**Instance attributes**: `_attempt_id`, `_runtime`, `_close_attempt`, `_audit`

<details><summary>Methods (9)</summary>

`__init__`, `advance_ready_tasks`, `_advance_generator_stage`, `_advance_evaluator_stage`, `_mark_launch_failed`, `_launch_ready_generator`, `_launch_evaluator`, `_start_evaluator_stage`, `_fresh_attempt`

</details>

---

## `task_center/attempt/state.py`

#### `AttemptStage`  ·  _enum_  ·  bases: `StrEnum`  ·  [L10]

Enumerates the sequential stages an Attempt moves through: plan, generate, evaluate, and closed.

**Enum members**: `PLAN = 'plan'`, `GENERATE = 'generate'`, `EVALUATE = 'evaluate'`, `CLOSED = 'closed'`

#### `AttemptStatus`  ·  _enum_  ·  bases: `StrEnum`  ·  [L17]

Enumerates the overall outcome status of an Attempt: running, passed, or failed.

**Enum members**: `RUNNING = 'running'`, `PASSED = 'passed'`, `FAILED = 'failed'`

#### `AttemptFailReason`  ·  _enum_  ·  bases: `StrEnum`  ·  [L23]

Enumerates the specific stage failure that caused an Attempt to fail.

**Enum members**: `PLANNER_FAILED = 'planner_failed'`, `GENERATOR_FAILED = 'generator_failed'`, `EVALUATOR_FAILED = 'evaluator_failed'`, `STARTUP_FAILED = 'startup_failed'`

#### `Attempt`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L31]

Immutable view of a persisted Attempt.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `iteration_id` | `str` |  |
| `attempt_sequence_no` | `int` |  |
| `stage` | `AttemptStage` |  |
| `status` | `AttemptStatus` |  |
| `planner_task_id` | `str \| None` |  |
| `plan_spec` | `str \| None` |  |
| `evaluation_criteria` | `tuple[str, ...]` |  |
| `generator_task_ids` | `tuple[str, ...]` |  |
| `evaluator_task_id` | `str \| None` |  |
| `deferred_goal_for_next_iteration` | `str \| None` |  |
| `fail_reason` | `AttemptFailReason \| None` |  |
| `created_at` | `datetime` |  |
| `updated_at` | `datetime` |  |
| `closed_at` | `datetime \| None` |  |

<details><summary>Methods (1)</summary>

`is_closed`

</details>

---

## `task_center/context_engine/context_outline.py`

#### `_OutlineNode`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L39]

One bullet's worth of data: tag, attrs, label, and any children.

**Fields**

| name | type | default |
|------|------|---------|
| `tag` | `str` |  |
| `attrs` | `dict[str, str]` |  |
| `descriptor` | `TagDescriptor` |  |
| `children` | `tuple['_OutlineNode', ...]` | `()` |

---

## `task_center/context_engine/core.py`

#### `ContextPacketStoreProtocol`  ·  _protocol_  ·  bases: `Protocol`  ·  [L47]

Protocol for stores that persist context packets, exposing an insert method returning the packet id.

<details><summary>Methods (1)</summary>

`insert`

</details>

#### `ContextEngineDeps`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L52]

Frozen bundle of stores recipes may read from.

**Fields**

| name | type | default |
|------|------|---------|
| `workflow_store` | `WorkflowStoreProtocol` |  |
| `iteration_store` | `IterationStoreProtocol` |  |
| `attempt_store` | `AttemptStoreProtocol` |  |
| `task_store` | `TaskStoreProtocol` |  |
| `context_packet_store` | `ContextPacketStoreProtocol \| None` | `None` |

#### `ContextEngine`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L69]

Routes recipe ids to registered builders.

**Fields**

| name | type | default |
|------|------|---------|
| `deps` | `ContextEngineDeps` |  |

<details><summary>Methods (1)</summary>

`build`

</details>

---

## `task_center/context_engine/exceptions.py`

#### `ContextEngineError`  ·  _exception_  ·  bases: `Exception`  ·  [L10]

Generic context engine failure.

#### `RecipeScopeError`  ·  _exception_  ·  bases: `ContextEngineError`  ·  [L14]

A recipe was called with a :class:`ContextScope` missing required fields.

#### `MissingContextRecipeError`  ·  _exception_  ·  bases: `ContextEngineError`  ·  [L18]

An agent definition was selected for composition but has no

#### `AgentDefinitionValidationError`  ·  _exception_  ·  bases: `ContextEngineError`  ·  [L23]

A registered :class:`AgentDefinition` references unknown or invalid

---

## `task_center/context_engine/packet.py`

#### `ContextPriority`  ·  _enum_  ·  bases: `StrEnum`  ·  [L18]

Block-level priority. Token budgets compress lower priorities first.

**Enum members**: `REQUIRED = 'required'`, `HIGH = 'high'`, `MEDIUM = 'medium'`, `LOW = 'low'`

#### `ContextBlockKind`  ·  _enum_  ·  bases: `StrEnum`  ·  [L30]

Convenience constants for known kinds. ``ContextBlock.kind`` accepts any string.

**Enum members**: `GOAL_STATEMENT = 'goal_statement'`, `ITERATION_STATEMENT = 'iteration_statement'`, `PRIOR_ITERATION_SUMMARY = 'prior_iteration_summary'`, `FAILED_ATTEMPT = 'failed_attempt'`, `PLANNED_TASK_SPEC = 'planned_task_spec'`, `TASK_SPECIFICATION = 'task_specification'`, `DEPENDENCY_SUMMARY = 'dependency_summary'`, `ENTRY_REQUEST = 'entry_request'`

#### `ContextRefs`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L43]

Canonical row references attached to every packet.

**Fields**

| name | type | default |
|------|------|---------|
| `workflow_id` | `str \| None` | `None` |
| `iteration_id` | `str \| None` | `None` |
| `attempt_id` | `str \| None` | `None` |
| `task_id` | `str \| None` | `None` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

#### `ContextBlock`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L54]

One typed unit of context surfaced to the model.

**Fields**

| name | type | default |
|------|------|---------|
| `kind` | `str` | `Field(min_length=1)` |
| `priority` | `ContextPriority` |  |
| `text` | `str` |  |
| `source_id` | `str \| None` | `None` |
| `source_kind` | `str \| None` | `None` |
| `metadata` | `dict[str, str]` | `Field(default_factory=dict)` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

<details><summary>Methods (1)</summary>

`_non_blank_required_text`

</details>

#### `ContextPacket`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L75]

A packet is the immutable output of a recipe build.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` | `Field(default_factory=lambda: str(uuid.uuid4()))` |
| `target_role` | `str` |  |
| `target_id` | `str \| None` | `None` |
| `canonical_refs` | `ContextRefs` |  |
| `blocks` | `list[ContextBlock]` | `Field(default_factory=list)` |
| `metadata` | `dict[str, str]` | `Field(default_factory=dict)` |
| `source_ids` | `list[str]` | `Field(default_factory=list)` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

---

## `task_center/context_engine/recipes_registry.py`

#### `ContextRecipe`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L25]

One registered recipe.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `required_scope_fields` | `frozenset[str]` |  |
| `build` | `RecipeBuild` |  |

#### `RecipeRegistry`  ·  _class_  ·  [L33]

Process-global recipe registry indexed by ``recipe.id``.

**Fields**

| name | type | default |
|------|------|---------|
| `_registry` | `ClassVar[dict[str, ContextRecipe]]` | `{}` |

<details><summary>Methods (5)</summary>

`register`, `get`, `has`, `list_ids`, `clear`

</details>

---

## `task_center/context_engine/renderer.py`

#### `XmlPromptRenderer`  ·  _class_  ·  [L54]

XML-tagged renderer.

<details><summary>Methods (8)</summary>

`render_context`, `_budget_from`, `_render_blocks`, `_render_block`, `_render_group`, `_tag_for`, `_validate_no_structural_closers`, `_compress`

</details>

---

## `task_center/context_engine/scope.py`

#### `ContextScope`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L26]

Identity surface threaded through resolver + engine + recipes.

**Fields**

| name | type | default |
|------|------|---------|
| `workflow_id` | `str \| None` | `None` |
| `iteration_id` | `str \| None` | `None` |
| `attempt_id` | `str \| None` | `None` |
| `task_id` | `str \| None` | `None` |

<details><summary>Methods (5)</summary>

`assert_fields`, `require_field`, `for_planner`, `for_generator`, `for_evaluator`

</details>

---

## `task_center/context_engine/tag_dictionary.py`

#### `TagDescriptor`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L27]

One row in the canonical tag dictionary.

**Fields**

| name | type | default |
|------|------|---------|
| `tag` | `str` |  |
| `attr_filter` | `dict[str, str] \| None` | `None` |
| `label` | `str` |  |

**Class variables**: `model_config = ConfigDict(frozen=True, extra='forbid')`

---

## `task_center/entry/bootstrap.py`

#### `TaskCenterEntryHandle`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L46]

Handle returned when starting a TaskCenter run, bundling run identifiers, sandbox binding, and the attempt launcher.

**Fields**

| name | type | default |
|------|------|---------|
| `request_id` | `str` |  |
| `task_center_run_id` | `str` |  |
| `binding` | `TaskCenterSandboxBinding` |  |
| `workflow_id` | `str` |  |
| `initial_iteration_id` | `str` |  |
| `initial_attempt_id` | `str` |  |
| `launcher` | `EphemeralAttemptAgentLauncher` |  |

<details><summary>Methods (1)</summary>

`sandbox_id`

</details>

#### `TaskCenterEntry`  ·  _class_  ·  [L90]

Bootstraps a top-level prompt into the normal Workflow lifecycle.

**Instance attributes**: `_config`, `_prompt`, `_sandbox_id`, `_on_agent_event`, `_task_store`, `_workflow_store`, `_iteration_store`, `_attempt_store`, `_runner`, `_context_packet_store`, `_sandbox_provisioner`

<details><summary>Methods (6)</summary>

`__init__`, `start`, `_create_top_level_run`, `_create_runtime`, `_build_composer`, `_finish_run_if_open`

</details>

---

## `task_center/entry/sandbox_provisioning.py`

#### `TaskCenterSandboxBinding`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L22]

Record binding one TaskCenter run to its sandbox, tracking ownership of that sandbox.

**Fields**

| name | type | default |
|------|------|---------|
| `sandbox_id` | `str` |  |
| `task_center_run_id` | `str` |  |
| `owned_by_task_center` | `bool` |  |

#### `TaskCenterSandboxProvisioner`  ·  _class_  ·  [L40]

Prepare the sandbox binding used by one TaskCenter run.

**Instance attributes**: `_create`, `_start`

<details><summary>Methods (2)</summary>

`__init__`, `prepare_for_run`

</details>

---

## `task_center/iteration/attempt_coordinator.py`

#### `IterationAttemptCoordinator`  ·  _class_  ·  [L54]

Coordinates attempts for one open Iteration.

**Instance attributes**: `iteration_id`, `_iteration_store`, `_attempt_store`, `_on_iteration_closed`, `_orchestrator_factory`, `_task_store`

<details><summary>Methods (19)</summary>

`__init__`, `create_initial_attempt`, `create_unstarted_initial_attempt`, `start_attempt`, `create_next_attempt`, `handle_attempt_closed`, `_current_iteration_snapshot`, `_insert_attempt`, `_start_orchestrator_if_configured`, `_close_attempt_after_startup_failure`, `_close_iteration_passed`, `_achieved_record_for`, `_retry_or_close_failed`, `_close_iteration_failed`, `_latest_failed_attempt_for`, `_emit_terminal_success`, `_emit_success_deferred`, `_emit_attempt_plan_failed`, `_build_prior_attempt_history`

</details>

#### `OpenIterationCoordinatorRegistry`  ·  _class_  ·  [L304]

In-memory registry enforcing one coordinator per open iteration.

**Instance attributes**: `_by_iteration_id`

<details><summary>Methods (4)</summary>

`__init__`, `register`, `get`, `deregister`

</details>

---

## `task_center/iteration/state.py`

#### `IterationStatus`  ·  _enum_  ·  bases: `StrEnum`  ·  [L13]

Enumeration of lifecycle states an Iteration can be in: open, succeeded, failed, or cancelled.

**Enum members**: `OPEN = 'open'`, `SUCCEEDED = 'succeeded'`, `FAILED = 'failed'`, `CANCELLED = 'cancelled'`

#### `IterationCreationReason`  ·  _enum_  ·  bases: `StrEnum`  ·  [L20]

Enumeration of why an Iteration was created: initial start or deferred-goal continuation.

**Enum members**: `INITIAL = 'initial'`, `DEFERRED_GOAL_CONTINUATION = 'partial_continuation'`

#### `Iteration`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L26]

Immutable view of a persisted Iteration.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `workflow_id` | `str` |  |
| `sequence_no` | `int` |  |
| `creation_reason` | `IterationCreationReason` |  |
| `goal` | `str` |  |
| `attempt_budget` | `int` |  |
| `status` | `IterationStatus` |  |
| `attempt_ids` | `tuple[str, ...]` |  |
| `deferred_goal_for_next_iteration` | `str \| None` |  |
| `created_at` | `datetime` |  |
| `updated_at` | `datetime` |  |
| `closed_at` | `datetime \| None` |  |
| `plan_spec` | `str \| None` | `None` |
| `task_summary` | `str \| None` | `None` |

<details><summary>Methods (4)</summary>

`is_open`, `attempt_count`, `has_budget_remaining`, `latest_attempt_id`

</details>

#### `FailedAttemptEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L67]

One past attempt's structural state.

**Fields**

| name | type | default |
|------|------|---------|
| `attempt_id` | `str` |  |
| `attempt_sequence_no` | `int` |  |
| `plan_spec` | `str \| None` |  |
| `evaluation_criteria` | `tuple[str, ...]` |  |
| `fail_reason` | `AttemptFailReason \| None` |  |

#### `TerminalSuccess`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L78]

Closure outcome marking an iteration as terminally succeeded with no further continuation needed.

**Fields**

| name | type | default |
|------|------|---------|
| `kind` | `Literal['terminal_success']` | `'terminal_success'` |

#### `SuccessDeferred`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L83]

Closure outcome signaling iteration success while deferring a remaining goal to the next iteration.

**Fields**

| name | type | default |
|------|------|---------|
| `deferred_goal_for_next_iteration` | `str` |  |
| `kind` | `Literal['success_deferred']` | `'success_deferred'` |

#### `AttemptPlanFailed`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L89]

Closure outcome representing an iteration whose attempt planning failed, carrying failure summary and prior attempt history.

**Fields**

| name | type | default |
|------|------|---------|
| `failure_summary` | `str` |  |
| `prior_attempt_history` | `tuple[FailedAttemptEntry, ...]` |  |
| `kind` | `Literal['attempt_plan_failed']` | `'attempt_plan_failed'` |

#### `IterationClosureReport`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L99]

Records the final outcome of a closed iteration, naming its final attempt and closure result.

**Fields**

| name | type | default |
|------|------|---------|
| `iteration_id` | `str` |  |
| `final_attempt_id` | `str` |  |
| `outcome` | `ClosureOutcome` |  |

---

## `task_center/submissions.py`

#### `PlannedGeneratorTask`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L16]

One normalized generator DAG node.

**Fields**

| name | type | default |
|------|------|---------|
| `local_id` | `str` |  |
| `agent_name` | `str` |  |
| `deps` | `tuple[str, ...]` |  |
| `task_spec` | `str` |  |

#### `PlannerSubmission`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L26]

Validated planner submission from a full or partial plan tool.

**Fields**

| name | type | default |
|------|------|---------|
| `attempt_id` | `str` |  |
| `planner_task_id` | `str` |  |
| `kind` | `Literal['completes', 'defers']` |  |
| `plan_spec` | `str` |  |
| `evaluation_criteria` | `tuple[str, ...]` |  |
| `tasks` | `tuple[PlannedGeneratorTask, ...]` |  |
| `deferred_goal_for_next_iteration` | `str \| None` |  |
| `summary` | `str` |  |

#### `PlannerFailureSubmission`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L40]

Runtime-synthesized planner failure.

**Fields**

| name | type | default |
|------|------|---------|
| `attempt_id` | `str` |  |
| `planner_task_id` | `str` |  |
| `fail_reason` | `Literal['run_exhausted']` |  |
| `summary` | `str` |  |

#### `GeneratorSubmission`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L50]

Validated terminal outcome for one generator task.

**Fields**

| name | type | default |
|------|------|---------|
| `attempt_id` | `str` |  |
| `task_id` | `str` |  |
| `outcome` | `Literal['success', 'failure', 'blocker']` |  |
| `summary` | `str` |  |
| `payload` | `dict[str, Any]` |  |

#### `EvaluatorSubmission`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L61]

Validated terminal outcome for one evaluator task.

**Fields**

| name | type | default |
|------|------|---------|
| `attempt_id` | `str` |  |
| `task_id` | `str` |  |
| `outcome` | `Literal['success', 'failure']` |  |
| `summary` | `str` |  |
| `payload` | `dict[str, Any]` |  |

---

## `task_center/workflow/closure_report_router.py`

#### `WorkflowClosureReportRouter`  ·  _class_  ·  [L24]

Single delivery path for final ``WorkflowClosureReport``s.

**Instance attributes**: `_runtime`

<details><summary>Methods (3)</summary>

`__init__`, `deliver`, `_deliver_entry_origin`

</details>

---

## `task_center/workflow/lifecycle.py`

#### `WorkflowLifecycle`  ·  _class_  ·  [L52]

Coordinates one workflow's iteration chain and closure report delivery.

**Instance attributes**: `_deliver_closure_report`, `_workflow_store`, `_iteration_store`, `_attempt_store`, `_iteration_coordinators`, `_config`, `_orchestrator_factory`, `_task_store`

<details><summary>Methods (11)</summary>

`__init__`, `create_workflow`, `create_initial_iteration_with_coordinator`, `create_deferred_iteration_with_coordinator`, `handle_iteration_closed`, `close_workflow`, `_require_workflow`, `_append_iteration_id`, `_insert_iteration_and_register_coordinator`, `_route_iteration_closure`, `_start_deferred_iteration`

</details>

---

## `task_center/workflow/starter.py`

#### `StartedWorkflow`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L38]

Result of starting a workflow, identifying its origin, initial iteration/attempt, and goal.

**Fields**

| name | type | default |
|------|------|---------|
| `origin` | `WorkflowOrigin` |  |
| `parent_attempt_id` | `str \| None` |  |
| `workflow_id` | `str` |  |
| `initial_iteration_id` | `str` |  |
| `initial_attempt_id` | `str` |  |
| `goal` | `str` |  |

<details><summary>Methods (1)</summary>

`parent_task_id`

</details>

#### `WorkflowStarter`  ·  _class_  ·  [L51]

Single orchestration entry point for prompt → workflow start.

**Instance attributes**: `_runtime`, `_orchestrator_factory`

<details><summary>Methods (9)</summary>

`__init__`, `start`, `_prepare_origin`, `_build_workflow_lifecycle`, `_assert_parent_running_and_no_open_child`, `_mark_parent_waiting`, `_compensate_failed_start`, `_restore_parent`, `_close_unstarted_attempt`

</details>

#### `_PreparedWorkflowOrigin`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L313]

Internal value resolving a workflow's run id and optional parent attempt before starting.

**Fields**

| name | type | default |
|------|------|---------|
| `task_center_run_id` | `str` |  |
| `parent_attempt_id` | `str \| None` |  |

---

## `task_center/workflow/state.py`

#### `WorkflowOriginKind`  ·  _enum_  ·  bases: `StrEnum`  ·  [L11]

Enumerates whether a workflow originated from an entry prompt or a task.

**Enum members**: `ENTRY = 'entry'`, `TASK = 'task'`

#### `WorkflowOrigin`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L17]

Where prompt text entered the workflow lifecycle.

**Fields**

| name | type | default |
|------|------|---------|
| `kind` | `WorkflowOriginKind` |  |
| `task_center_run_id` | `str \| None` | `None` |
| `task_id` | `str \| None` | `None` |

<details><summary>Methods (3)</summary>

`entry`, `task`, `__post_init__`

</details>

#### `WorkflowStatus`  ·  _enum_  ·  bases: `StrEnum`  ·  [L44]

Enumeration of the lifecycle states a workflow can occupy (open, succeeded, failed, cancelled).

**Enum members**: `OPEN = 'open'`, `SUCCEEDED = 'succeeded'`, `FAILED = 'failed'`, `CANCELLED = 'cancelled'`

#### `Workflow`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L52]

Immutable view of a persisted Workflow.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `task_center_run_id` | `str` |  |
| `goal` | `str` |  |
| `status` | `WorkflowStatus` |  |
| `iteration_ids` | `tuple[str, ...]` |  |
| `final_outcome` | `dict[str, Any] \| None` |  |
| `created_at` | `datetime` |  |
| `updated_at` | `datetime` |  |
| `closed_at` | `datetime \| None` |  |
| `origin_kind` | `WorkflowOriginKind` | `WorkflowOriginKind.TASK` |
| `requested_by_task_id` | `str \| None` | `None` |

<details><summary>Methods (2)</summary>

`is_open`, `origin`

</details>

#### `WorkflowClosureReport`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L81]

Final report emitted when a workflow closes.

**Fields**

| name | type | default |
|------|------|---------|
| `workflow_id` | `str` |  |
| `task_center_run_id` | `str` |  |
| `origin_kind` | `WorkflowOriginKind` |  |
| `requested_by_task_id` | `str \| None` |  |
| `outcome` | `Literal['success', 'failed']` |  |
| `final_iteration_id` | `str` |  |
| `final_attempt_id` | `str \| None` |  |

<details><summary>Methods (1)</summary>

`to_final_outcome`

</details>

#### `WorkflowClosureDeliveryResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L108]

Result describing the delivery status of a workflow closure report back to its requesting task or parent attempt.

**Fields**

| name | type | default |
|------|------|---------|
| `status` | `WorkflowClosureDeliveryStatus` |  |
| `requested_by_task_id` | `str \| None` |  |
| `parent_attempt_id` | `str \| None` |  |


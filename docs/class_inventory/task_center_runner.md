# Module `task_center_runner` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/task_center_runner/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**150 classes across 84 files.**

task_center_runner is the deterministic test and benchmark harness that drives EphemeralOS agent runs end-to-end. Its center of gravity is a large suite of Scenario/ScenarioBase classes under scenarios/* that exercise the full pipeline — attempt retry and budget-exhaustion paths, dependency DAGs (serial/parallel/diamond/mixed), nested workflows, planner-validation failures (cycles, duplicate ids, unknown deps), and heavy sandbox cases (background_shell, ephemeral_workspace, OCC concurrency, plugins, and complex project builds), plus capacity matrices. Supporting this are the mock scripted-agent execution layer (agent/mock/*: MockSquadRunner, ScenarioLoopRunner, ScenarioEventSource, probes, and prepared tool scripts that call real tools) and the path-agnostic core/* run scaffolding (RunConfig/RunContext, LifecycleHooks, SandboxProvisioner, and report types) that selects either the scripted or real-LLM path. Rounding it out are the audit/* subsystem (event bus, recorder, metrics aggregation, and rotating JSONL/sandbox sinks) and the benchmarks/sweevo/* SWE-EVO integration (instances, provisioner, lifecycle, and results).

## Contents

- **`task_center_runner/agent/mock/capacity_actions/types.py`** — `CapacityActionResult`
- **`task_center_runner/agent/mock/complex_project_build_grep_glob_probe.py`** — `GrepGlobStats`
- **`task_center_runner/agent/mock/complex_project_build_probe.py`** — `ProbeStats`, `ProbeContext`, `_SharedAttemptBootstrap`
- **`task_center_runner/agent/mock/complex_project_build_shell_edit_lsp_probe.py`** — `ShellEditLspStats`
- **`task_center_runner/agent/mock/event_source.py`** — `ToolCall`, `Turn`, `ScenarioEventSource`
- **`task_center_runner/agent/mock/probe_bridge.py`** — `_CallToolBridge`
- **`task_center_runner/agent/mock/probes.py`** — `ProbeContext`
- **`task_center_runner/agent/mock/prompt_inspector.py`** — `PromptInspection`, `LaunchRecord`, `ToolCallRecord`
- **`task_center_runner/agent/mock/runner.py`** — `MockSquadRunner`
- **`task_center_runner/agent/mock/sandbox_probe.py`** — `SandboxCheck`
- **`task_center_runner/agent/mock/scenario_loop_runner.py`** — `_UnusedApiClient`, `ScenarioLoopRunner`
- **`task_center_runner/agent/mock/tool_scripts.py`** — `CallTool`, `ToolScriptStep`, `PreparedToolScript`, `ToolScriptResult`, `PreparedToolScriptEngine`
- **`task_center_runner/audit/bus.py`** — `AuditEventBus`
- **`task_center_runner/audit/daemon_pull.py`** — `PullerStats`, `DaemonAuditPuller`
- **`task_center_runner/audit/events.py`** — `EventType`, `Event`
- **`task_center_runner/audit/legacy.py`** — `LegacySandboxAuditSink`
- **`task_center_runner/audit/metrics.py`** — `_ToolStart`, `_PerTool`, `MetricsAggregator`
- **`task_center_runner/audit/node_id.py`** — `NodeId`
- **`task_center_runner/audit/recorder.py`** — `_ListenerHandle`, `AuditRecorder`
- **`task_center_runner/audit/sandbox_events_sink.py`** — `RotatingJsonlSink`
- **`task_center_runner/benchmarks/sweevo/_snapshot.py`** — `SnapshotNotRegisteredError`
- **`task_center_runner/benchmarks/sweevo/eval.py`** — `_TestSetOutcome`, `SweevoLifecycle`
- **`task_center_runner/benchmarks/sweevo/models.py`** — `SWEEvoInstance`, `SWEEvoResult`, `PreContext`
- **`task_center_runner/benchmarks/sweevo/run.py`** — `SweevoProvisioner`
- **`task_center_runner/core/config.py`** — `RunConfig`, `RunContext`
- **`task_center_runner/core/lifecycle.py`** — `LifecycleHooks`, `NoopLifecycle`
- **`task_center_runner/core/real_agent_run.py`** — `RealAgentRunReport`
- **`task_center_runner/core/report.py`** — `PipelineReport`
- **`task_center_runner/core/runner.py`** — `RunReport`
- **`task_center_runner/core/sandbox.py`** — `SandboxLease`, `SandboxProvisioner`, `AttachExisting`
- **`task_center_runner/core/stores.py`** — `TaskCenterStoreBundle`
- **`task_center_runner/environments/sweevo_image/fixtures.py`** — `_SweevoSessionLock`
- **`task_center_runner/hooks/registry.py`** — `HookResult`, `MutableMockState`, `Hook`, `HookSet`
- **`task_center_runner/scenarios/base.py`** — `ToolCallSpec`, `ScenarioContext`, `Scenario`, `ScenarioBase`
- **`task_center_runner/scenarios/capacity/full_system_capacity_matrix.py`** — `FullSystemCapacityMatrix`
- **`task_center_runner/scenarios/capacity/pack_catalog.py`** — `CapacityPackSpec`
- **`task_center_runner/scenarios/correctness_testing.py`** — `CorrectnessTesting`
- **`task_center_runner/scenarios/full_case_user_input.py`** — `FullCaseUserInput`
- **`task_center_runner/scenarios/full_stack_adversarial.py`** — `FullStackCell`, `FullStackAdversarial`
- **`task_center_runner/scenarios/lifecycle.py`** — `ScenarioLifecycle`
- **`task_center_runner/scenarios/pipeline/attempt_budget_exhausted.py`** — `AttemptBudgetExhausted`
- **`task_center_runner/scenarios/pipeline/attempt_retry_evaluator_failure.py`** — `AttemptRetryEvaluatorFailure`
- **`task_center_runner/scenarios/pipeline/attempt_retry_generator_failure.py`** — `AttemptRetryGeneratorFailure`
- **`task_center_runner/scenarios/pipeline/attempt_retry_planner_failure.py`** — `AttemptRetryPlannerFailure`
- **`task_center_runner/scenarios/pipeline/deferred_parent_planner_terminal_routing.py`** — `DeferredParentPlannerTerminalRouting`
- **`task_center_runner/scenarios/pipeline/dependency_blocked_descendants.py`** — `DependencyBlockedDescendants`
- **`task_center_runner/scenarios/pipeline/dependency_dag_diamond.py`** — `DependencyDagDiamond`
- **`task_center_runner/scenarios/pipeline/dependency_dag_mixed.py`** — `DependencyDagMixed`
- **`task_center_runner/scenarios/pipeline/dependency_dag_parallel.py`** — `DependencyDagParallel`
- **`task_center_runner/scenarios/pipeline/dependency_dag_serial.py`** — `DependencyDagSerial`
- **`task_center_runner/scenarios/pipeline/generator_failure_quiescence.py`** — `GeneratorFailureQuiescence`
- **`task_center_runner/scenarios/pipeline/initial_messages_capture.py`** — `InitialMessagesCapture`
- **`task_center_runner/scenarios/pipeline/initial_workflow.py`** — `InitialWorkflow`
- **`task_center_runner/scenarios/pipeline/iterative_deferral.py`** — `IterativeDeferral`
- **`task_center_runner/scenarios/pipeline/nested_workflow.py`** — `NestedWorkflow`, `NestedWorkflowFailure`
- **`task_center_runner/scenarios/planner_validation/cycle_in_deps.py`** — `PlannerCycleInDeps`
- **`task_center_runner/scenarios/planner_validation/defers_without_deferred_goal.py`** — `PlannerDefersWithoutDeferredGoal`
- **`task_center_runner/scenarios/planner_validation/duplicate_local_id.py`** — `PlannerDuplicateLocalId`
- **`task_center_runner/scenarios/planner_validation/empty_tasks.py`** — `PlannerEmptyTasks`
- **`task_center_runner/scenarios/planner_validation/unknown_agent_name.py`** — `PlannerUnknownAgentName`
- **`task_center_runner/scenarios/planner_validation/unknown_dep.py`** — `PlannerUnknownDep`
- **`task_center_runner/scenarios/sandbox/_fixtures/lsp_expectations.py`** — `LspExpectation`
- **`task_center_runner/scenarios/sandbox/_fixtures/refactor_passes.py`** — `RefactorEdit`, `LSPRefSpec`, `RefactorPass`
- **`task_center_runner/scenarios/sandbox/_fixtures/scheduler_demo_data.py`** — `Patch`, `FixtureFile`
- **`task_center_runner/scenarios/sandbox/auto_squash_commit_resume.py`** — `AutoSquashCommitResume`
- **`task_center_runner/scenarios/sandbox/background_shell.py`** — `_BackgroundShellScenarioBase`, `BackgroundShellGolden`, `BackgroundShellStop`, `BackgroundShellInterleave`, `BackgroundShellExhaustion`, `BackgroundShellPartialWriteCancel`, `BackgroundShellStopDuringMaintenance`, `BackgroundShellLateCancelRace`, `BackgroundMixedFgBgSamePathConflict`, `BackgroundHeartbeatLossReapsOnlyStaleBg`, `BackgroundExitIwsDrainsAgentTasks`, `BackgroundEngineRestartNoLeaseLeak`, `BackgroundManySmallWritesDoNotStarveDispatcher`, `BackgroundMixedOpConcurrent`
- **`task_center_runner/scenarios/sandbox/complex_project_build.py`** — `ComplexProjectBuild`, `ComplexProjectBuildSmoke`
- **`task_center_runner/scenarios/sandbox/complex_project_build_grep_glob.py`** — `ComplexProjectBuildGrepGlob`, `ComplexProjectBuildGrepGlobSmoke`
- **`task_center_runner/scenarios/sandbox/complex_project_build_shell_edit_lsp.py`** — `ComplexProjectBuildShellEditLsp`, `ComplexProjectBuildShellEditLspSmoke`
- **`task_center_runner/scenarios/sandbox/ephemeral_workspace.py`** — `_EphemeralWorkspaceScenarioBase`, `EphemeralWorkspaceAllVerbs`, `EphemeralWorkspaceConcurrentWrites`, `EphemeralWorkspaceSamePathConflict`, `EphemeralWorkspacePolicy`, `EphemeralWorkspaceCancellation`, `EphemeralWorkspaceO1Disk`
- **`task_center_runner/scenarios/sandbox/heavy_io_zoned_concurrent.py`** — `HeavyIoZonedConcurrent`
- **`task_center_runner/scenarios/sandbox/high_concurrency_layerstack_overlay_occ.py`** — `HighConcurrencyLayerstackOverlayOcc`
- **`task_center_runner/scenarios/sandbox/occ_concurrent_conflicts.py`** — `OccConcurrentConflicts`
- **`task_center_runner/scenarios/sandbox/plugin.py`** — `_PluginScenarioBase`, `PluginReadOnlyLspRefresh`, `PluginWriteAllowedPublish`, `PluginIntentContract`, `PluginIwsPolicy`, `PluginSetupFailure`, `PluginServiceEvict`
- **`task_center_runner/scenarios/user_input.py`** — `RequirementItem`, `WorkPackage`, `UserInputPlan`
- **`task_center_runner/tests/mock/_focused_scenario_contracts.py`** — `FocusedScenarioCase`
- **`task_center_runner/tests/mock/_project_build_contracts.py`** — `ComplexBuildContract`, `ShellEditLspContract`, `GrepGlobContract`
- **`task_center_runner/tests/mock/contracts/test_scenario_event_source_spike.py`** — `_UnusedClient`
- **`task_center_runner/tests/mock/contracts/test_scenario_loop_runner_planner_submit.py`** — `_PlannerSubmitProof`
- **`task_center_runner/tests/mock/sandbox/isolated_workspace/_iws_fixtures.py`** — `SentinelFile`
- **`task_center_runner/tests/mock/sandbox/isolated_workspace/_iws_invariants.py`** — `LatencyBudget`
- **`task_center_runner/tests/mock/sandbox/isolated_workspace/pre_flight/test_phase_timer_invariants.py`** — `_FakeClock`
- **`task_center_runner/tests/mock/sandbox/project_build/test_complex_project_build_fixtures.py`** — `_ProbeCtx`
- **`task_center_runner/tests/mock/sandbox/project_build/test_project_build_shell_edit_lsp_three_parallel_agents.py`** — `ComplexProjectBuildShellEditLspThreeParallelAgents`

---

## `task_center_runner/agent/mock/capacity_actions/types.py`

#### `CapacityActionResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L10]

Summary returned by capacity action drivers.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `summary` | `str` |  |
| `artifact_path` | `str \| None` |  |
| `expected_errors` | `tuple[str, ...]` |  |
| `counters` | `Mapping[str, int \| float \| str]` |  |

---

## `task_center_runner/agent/mock/complex_project_build_grep_glob_probe.py`

#### `GrepGlobStats`  ·  _dataclass_  ·  bases: `ProbeStats`  ·  decorators: `@dataclass`  ·  [L66]

Accumulates statistics for the mock grep/glob/edit search probe runs.

**Fields**

| name | type | default |
|------|------|---------|
| `grep_count` | `int` | `0` |
| `glob_count` | `int` | `0` |
| `grep_matches` | `int` | `0` |
| `glob_matches` | `int` | `0` |
| `search_checks` | `int` | `0` |
| `search_failures` | `int` | `0` |
| `negative_grep_checks` | `int` | `0` |
| `grep_mode_counts` | `dict[str, int] \| None` | `None` |

<details><summary>Methods (1)</summary>

`mode_counts`

</details>

---

## `task_center_runner/agent/mock/complex_project_build_probe.py`

#### `ProbeStats`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L91]

Counters tracked during a probe run for §7 assertions and metrics.

**Fields**

| name | type | default |
|------|------|---------|
| `write_count` | `int` | `0` |
| `edit_count` | `int` | `0` |
| `read_count` | `int` | `0` |
| `shell_count` | `int` | `0` |
| `lsp_counts` | `dict[str, int]` | `field(default_factory=dict)` |
| `api_read_count` | `int` | `0` |
| `api_edit_count` | `int` | `0` |
| `api_shell_count` | `int` | `0` |
| `intentional_conflicts` | `int` | `0` |
| `tool_call_metadata` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `phases` | `list[dict[str, Any]]` | `field(default_factory=list)` |

<details><summary>Methods (1)</summary>

`edit_to_write_ratio`

</details>

#### `ProbeContext`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L113]

Bundles the runner helpers the probe needs.

**Fields**

| name | type | default |
|------|------|---------|
| `metadata` | `ExecutionMetadata` |  |
| `emit` | `EmitStreamEvent` |  |
| `call_tool` | `CallTool` |  |
| `publish` | `PublishEvent` |  |
| `publish_mock_record` | `PublishMockRecord` |  |
| `record_tool_check` | `RecordToolCheck` |  |
| `caller` | `SandboxCaller` |  |
| `sandbox_id` | `str` |  |
| `smoke` | `bool` |  |

#### `_SharedAttemptBootstrap`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L134]

Coordinates a shared one-time workspace reset across concurrent attempts via a barrier condition.

**Fields**

| name | type | default |
|------|------|---------|
| `expected` | `int` |  |
| `condition` | `asyncio.Condition` | `field(default_factory=asyncio.Condition)` |
| `arrived` | `int` | `0` |
| `reset_started` | `bool` | `False` |
| `reset_done` | `bool` | `False` |
| `reset_error` | `Exception \| None` | `None` |

---

## `task_center_runner/agent/mock/complex_project_build_shell_edit_lsp_probe.py`

#### `ShellEditLspStats`  ·  _dataclass_  ·  bases: `ProbeStats`  ·  decorators: `@dataclass`  ·  [L93]

Accumulates statistics for the mock shell-edit and LSP semantic-check probe runs.

**Fields**

| name | type | default |
|------|------|---------|
| `logical_edit_count` | `int` | `0` |
| `edit_file_edit_count` | `int` | `0` |
| `shell_edit_count` | `int` | `0` |
| `shell_edit_errors` | `int` | `0` |
| `shell_edit_payloads` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `shell_edit_tool_metadata` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `shell_edit_wall_seconds` | `list[float]` | `field(default_factory=list)` |
| `lsp_semantic_checks` | `dict[str, int]` | `field(default_factory=dict)` |
| `lsp_semantic_failures` | `int` | `0` |
| `diagnostic_error_detected` | `bool` | `False` |
| `diagnostic_repair_cleared` | `bool` | `False` |
| `diagnostic_probe_checks` | `int` | `0` |

<details><summary>Methods (2)</summary>

`shell_edit_ratio`, `total_lsp_semantic_checks`

</details>

---

## `task_center_runner/agent/mock/event_source.py`

#### `ToolCall`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L57]

One tool the scripted agent calls this turn.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `input` | `dict` | `field(default_factory=dict)` |

#### `Turn`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L65]

One scripted assistant turn: optional thinking/text + zero or more calls.

**Fields**

| name | type | default |
|------|------|---------|
| `calls` | `tuple[ToolCall, ...]` | `()` |
| `thinking` | `str \| None` | `None` |
| `text` | `str \| None` | `None` |

#### `ScenarioEventSource`  ·  _class_  ·  [L103]

Per-agent scripted event source. Holds one live turn-coroutine.

**Instance attributes**: `_script`, `_script_builder`, `_agent_name`, `_run_id`, `_primed`

<details><summary>Methods (3)</summary>

`__init__`, `__call__`, `_advance`

</details>

---

## `task_center_runner/agent/mock/probe_bridge.py`

#### `_CallToolBridge`  ·  _class_  ·  [L51]

Provides the bridging ``call_tool`` + a request queue the driver drains.

**Class variables**: `__slots__ = ('_queue',)`

**Instance attributes**: `_queue`

<details><summary>Methods (2)</summary>

`__init__`, `call_tool`

</details>

---

## `task_center_runner/agent/mock/probes.py`

#### `ProbeContext`  ·  _class_  ·  [L35]

Out-of-band sandbox helpers a probe coroutine needs.

**Instance attributes**: `_metadata`, `_repo_dir`, `_bus`, `_sink`

<details><summary>Methods (14)</summary>

`__init__`, `metadata`, `probe_path`, `_absolute_probe_path`, `_require_sandbox_id`, `_caller`, `_publish`, `publish`, `publish_mock_record`, `_publish_check`, `record_check`, `assert_read_contains`, `run_batch_edit`, `run_expected_conflict`

</details>

---

## `task_center_runner/agent/mock/prompt_inspector.py`

#### `PromptInspection`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L10]

Records the outcome of inspecting an agent prompt, holding per-check pass/fail results and justification.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `agent_name` | `str` |  |
| `role` | `str` |  |
| `checks` | `dict[str, bool]` |  |
| `justification` | `str` |  |

<details><summary>Methods (2)</summary>

`passed`, `as_dict`

</details>

#### `LaunchRecord`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L28]

Immutable record capturing a single mock agent launch's task, attempt, agent name, role, and prompt preview.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `attempt_id` | `str \| None` |  |
| `agent_name` | `str` |  |
| `role` | `str` |  |
| `prompt_preview` | `str` |  |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `ToolCallRecord`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L40]

Immutable record capturing one observed tool call's task, tool name, error flag, and metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `tool_name` | `str` |  |
| `is_error` | `bool` |  |
| `metadata` | `dict[str, Any]` | `field(default_factory=dict)` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

---

## `task_center_runner/agent/mock/runner.py`

#### `MockSquadRunner`  ·  _class_  ·  [L161]

Deterministic agent execution handlers that call real tools.

**Instance attributes**: `_repo_dir`, `_bus`, `_sandbox_audit_sink`, `_task_center_run_id`, `_scenario`, `_mutable_state`, `_audit_recorder`, `_script_engine`

<details><summary>Methods (45)</summary>

`__init__`, `bind_audit_recorder`, `__call__`, `_metadata_for`, `_approve_terminal`, `_run_planner`, `_run_executor`, `_run_verifier`, `_run_evaluator`, `_scenario_context`, `_run_preflight_probe`, `_run_sandbox_integrity_probe`, `_run_batch_edit`, `_run_expected_conflict`, `_run_auto_squash_commit_resume_probe`, `_run_complex_project_build_probe`, `_run_high_concurrency_seed_probe`, `_run_high_concurrency_worker_probe`, `_run_high_concurrency_reconcile_probe`, `_run_heavy_io_zoned_seed_probe`, `_run_heavy_io_zoned_worker_probe`, `_run_heavy_io_zoned_reconcile_probe`, `_run_background_shell_probe`, `_run_ephemeral_workspace_probe`, `_run_plugin_workspace_probe`, `_run_complex_project_build_shell_edit_lsp_probe`, `_run_complex_project_build_grep_glob_probe`, `_run_final_probe`, `_call_tool`, `_record_tool_check` _(+15 more)_

</details>

---

## `task_center_runner/agent/mock/sandbox_probe.py`

#### `SandboxCheck`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L10]

Immutable result of one sandbox probe assertion with pass/fail status, detail, and changed paths.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `passed` | `bool` |  |
| `detail` | `str` |  |
| `changed_paths` | `tuple[str, ...]` | `()` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

---

## `task_center_runner/agent/mock/scenario_loop_runner.py`

#### `_UnusedApiClient`  ·  _class_  ·  [L48]

``event_source`` short-circuits the loop's provider call, so the api_client

<details><summary>Methods (1)</summary>

`aclose`

</details>

#### `ScenarioLoopRunner`  ·  _class_  ·  [L67]

Drop-in ``AttemptAgentRunner`` (same call signature as run_ephemeral_agent).

**Instance attributes**: `_repo_dir`, `_bus`, `_scenario`, `_mutable_state`, `_audit_recorder`

<details><summary>Methods (10)</summary>

`__init__`, `bind_audit_recorder`, `_event_source_factory`, `__call__`, `_publish_launch`, `_publish_tool_call`, `_publish_record`, `_publish_prompt_inspection`, `_inspect_prompt`, `_record_initial_messages`

</details>

---

## `task_center_runner/agent/mock/tool_scripts.py`

#### `CallTool`  ·  _protocol_  ·  bases: `Protocol`  ·  [L32]

Protocol for an async callable that executes a tool and returns its result.

<details><summary>Methods (1)</summary>

`__call__`

</details>

#### `ToolScriptStep`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L54]

One concrete tool call inside a prepared script.

**Fields**

| name | type | default |
|------|------|---------|
| `label` | `str` |  |
| `tool` | `BaseTool` |  |
| `args` | `dict[str, Any]` |  |
| `expect_error` | `bool` | `False` |

#### `PreparedToolScript`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L64]

A deterministic sequence of real tool calls.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `summary` | `str` |  |
| `artifact` | `str` |  |
| `steps` | `tuple[ToolScriptStep, ...]` |  |

#### `ToolScriptResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L74]

Result summary returned after a prepared script runs.

**Fields**

| name | type | default |
|------|------|---------|
| `script_name` | `str` |  |
| `summary` | `str` |  |
| `artifact` | `str` |  |
| `results` | `tuple[ToolResult, ...]` |  |

#### `PreparedToolScriptEngine`  ·  _class_  ·  [L83]

Execute prepared scripts through the same tool path real agents use.

**Instance attributes**: `_call_tool`

<details><summary>Methods (2)</summary>

`__init__`, `run`

</details>

---

## `task_center_runner/audit/bus.py`

#### `AuditEventBus`  ·  _class_  ·  [L12]

Synchronous fanout bus. Single-threaded; no locking.

**Instance attributes**: `_handlers`, `errors`

<details><summary>Methods (3)</summary>

`__init__`, `publish`, `subscribe`

</details>

---

## `task_center_runner/audit/daemon_pull.py`

#### `PullerStats`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L45]

Mutable accumulator tracking event-puller statistics like pull counts, dropped events, cursors, and latency samples.

**Fields**

| name | type | default |
|------|------|---------|
| `pull_count` | `int` | `0` |
| `empty_pull_count` | `int` | `0` |
| `events_pulled` | `int` | `0` |
| `pull_error_count` | `int` | `0` |
| `dropped_event_count` | `int` | `0` |
| `lost_before_seq` | `int` | `0` |
| `max_buffer_pressure` | `float` | `0.0` |
| `final_cursor` | `int` | `-1` |
| `floor_raises` | `int` | `0` |
| `daemon_restarts_observed` | `int` | `0` |
| `pull_ms_samples` | `list[float]` | `field(default_factory=list)` |

<details><summary>Methods (3)</summary>

`record_pull_ms`, `percentiles`, `as_dict`

</details>

#### `DaemonAuditPuller`  ·  _class_  ·  [L97]

Polls the daemon audit ring at an adaptive cadence.

**Instance attributes**: `_default_floor_ms`, `_floor_ms`, `_active_target_ms`, `_idle_target_ms`, `_isolated_target_ms`, `_pressure_target_ms`, `_pull`, `_emit`, `_pull_limit`, `_cursor`, `_known_epoch_id`, `_stats`, `_pressure_streak`, `_isolated_active`, `_has_inflight`, `_task`, `_stop_event`, `_stopped`

<details><summary>Methods (15)</summary>

`__init__`, `start`, `stop`, `set_isolated_active`, `set_inflight`, `reset_floor`, `stats`, `floor_ms`, `_run`, `_pull_once`, `_final_drain`, `_observe_buffer`, `_observe_epoch`, `_escalate_floor`, `_compute_interval_ms`

</details>

---

## `task_center_runner/audit/events.py`

#### `EventType`  ·  _enum_  ·  bases: `StrEnum`  ·  [L44]

All audit event kinds. Plan §8.

**Enum members**: `RUN_STARTED = 'run_started'`, `RUN_COMPLETED = 'run_completed'`, `WORKFLOW_STARTED = 'workflow_started'`, `WORKFLOW_COMPLETED = 'workflow_completed'`, `WORKFLOW_REQUESTED = 'workflow_requested'`, `ITERATION_STARTED = 'iteration_started'`, `ITERATION_COMPLETED = 'iteration_completed'`, `ITERATION_FROM_DEFERRED_GOAL_CREATED = 'iteration_continuation_created'`, `ATTEMPT_STARTED = 'attempt_started'`, `ATTEMPT_PASSED = 'attempt_passed'`, `ATTEMPT_FAILED = 'attempt_failed'`, `PLANNER_INVOKED = 'planner_invoked'`, `PLANNER_COMPLETES_GOAL_PLAN = 'planner_full_plan'`, `PLANNER_DEFERS_GOAL_PLAN = 'planner_partial_plan'`, `PLANNER_REPLAN = 'planner_replan'`, `EXECUTOR_INVOKED = 'executor_invoked'`, `EXECUTOR_SUCCESS = 'executor_success'`, `EXECUTOR_FAILURE = 'executor_failure'`, `VERIFIER_INVOKED = 'verifier_invoked'`, `VERIFIER_SUCCESS = 'verifier_success'`, `VERIFIER_FAILURE = 'verifier_failure'`, `EVALUATOR_INVOKED = 'evaluator_invoked'`, `EVALUATOR_SUCCESS = 'evaluator_success'`, `EVALUATOR_FAILURE = 'evaluator_failure'`, `RECURSIVE_WORKFLOW_REQUESTED = 'recursive_workflow_requested'`, `RECURSIVE_WORKFLOW_COMPLETED = 'recursive_workflow_completed'`, `FULL_STACK_SCRIPT_COMPLETED = 'full_stack_script_completed'`, `TOOL_CALL_STARTED = 'tool_call_started'`, `TOOL_CALL_COMPLETED = 'tool_call_completed'`, `TOOL_CALL_ERROR = 'tool_call_error'`, `SANDBOX_WRITE_COMMITTED = 'sandbox_write_committed'`, `SANDBOX_EDIT_COMMITTED = 'sandbox_edit_committed'`, `SANDBOX_SHELL_COMMITTED = 'sandbox_shell_committed'`, `SANDBOX_BATCH_EDIT_APPLIED = 'sandbox_batch_edit_applied'`, `SANDBOX_CONFLICT_DETECTED = 'sandbox_conflict_detected'`, `SANDBOX_LAYER_STACK_LEASE_ACQUIRED = 'sandbox_layer_stack_lease_acquired'`, `SANDBOX_LAYER_STACK_LAYER_CREATED = 'sandbox_layer_stack_layer_created'`, `SANDBOX_LAYER_STACK_LAYERS_SQUASHED = 'sandbox_layer_stack_layers_squashed'`, `SANDBOX_OVERLAY_EXECUTED = 'pipeline_executed'`, `SANDBOX_OCC_CHANGESET_RECEIVED = 'sandbox_occ_changeset_received'`, `SANDBOX_OCC_CHANGES_COMMITTED = 'sandbox_occ_changes_committed'`, `SANDBOX_RESOURCE_SNAPSHOT = 'sandbox_resource_snapshot'`, `SANDBOX_TOOL_CANCELLED = 'sandbox_tool_cancelled'`, `SANDBOX_ISOLATED_WORKSPACE_ENTER = 'sandbox_isolated_workspace_enter'`, `SANDBOX_ISOLATED_WORKSPACE_EXIT = 'sandbox_isolated_workspace_exit'`, `SANDBOX_ISOLATED_WORKSPACE_TOOL_CALL = 'sandbox_isolated_workspace_tool_call'`, `SANDBOX_ISOLATED_WORKSPACE_EVICTED = 'sandbox_isolated_workspace_evicted'`, `SANDBOX_ISOLATED_WORKSPACE_GC_ORPHAN = 'sandbox_isolated_workspace_gc_orphan'`, `HOOK_INJECTED_FAILURE = 'hook_injected_failure'`, `HOOK_ASSERTED = 'hook_asserted'`, `MOCK_LAUNCH_RECORDED = 'mock_launch_recorded'`, `MOCK_TOOL_CALL_RECORDED = 'mock_tool_call_recorded'`, `MOCK_PROMPT_INSPECTED = 'mock_prompt_inspected'`, `MOCK_SANDBOX_CHECK_RECORDED = 'mock_sandbox_check_recorded'`

#### `Event`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L118]

One audit event.

**Fields**

| name | type | default |
|------|------|---------|
| `type` | `EventType` |  |
| `node` | `NodeId` |  |
| `payload` | `dict[str, Any]` | `field(default_factory=dict)` |
| `correlation_id` | `str \| None` | `None` |
| `ts` | `datetime` | `field(default_factory=lambda: datetime.now(UTC))` |

---

## `task_center_runner/audit/legacy.py`

#### `LegacySandboxAuditSink`  ·  _class_  ·  bases: `AuditSink`  ·  [L34]

Forward sandbox-owned audit events into the legacy live-e2e bus.

**Instance attributes**: `_bus`

<details><summary>Methods (2)</summary>

`__init__`, `publish`

</details>

---

## `task_center_runner/audit/metrics.py`

#### `_ToolStart`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L100]

Lightweight record marking a tool call's start time, node, and input preview for latency tracking.

**Fields**

| name | type | default |
|------|------|---------|
| `ts` | `datetime` |  |
| `node` | `dict[str, Any]` |  |
| `input_keys` | `list[str]` |  |
| `input_preview` | `str \| None` |  |

#### `_PerTool`  ·  _class_  ·  [L107]

Mutable per-tool counters/latencies.

**Class variables**: `__slots__ = ('count', 'errors', 'latencies_ms', 'samples')`

**Instance attributes**: `count`, `errors`, `latencies_ms`, `samples`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `MetricsAggregator`  ·  _class_  ·  [L119]

Aggregate tool-call counts, errors, and latency percentiles.

**Instance attributes**: `_per_tool`, `_open_starts`

<details><summary>Methods (8)</summary>

`__init__`, `observe`, `snapshot`, `performance_snapshot`, `_slowest`, `_pop_start`, `_key`, `_sample`

</details>

---

## `task_center_runner/audit/node_id.py`

#### `NodeId`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L21]

Hierarchical breadcrumb identifying where in the run an event occurred.

**Fields**

| name | type | default |
|------|------|---------|
| `task_center_run_id` | `str` |  |
| `workflow_id` | `str \| None` | `None` |
| `workflow_seq` | `int \| None` | `None` |
| `iteration_id` | `str \| None` | `None` |
| `iteration_seq` | `int \| None` | `None` |
| `attempt_id` | `str \| None` | `None` |
| `attempt_seq` | `int \| None` | `None` |
| `agent_role` | `PrimaryRole \| None` | `None` |
| `agent_name` | `str \| None` | `None` |
| `agent_run_id` | `str \| None` | `None` |
| `tool_name` | `str \| None` | `None` |

---

## `task_center_runner/audit/recorder.py`

#### `_ListenerHandle`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L180]

Bookkeeping for a single ``sqlalchemy.event.listens_for`` registration.

**Fields**

| name | type | default |
|------|------|---------|
| `target` | `Any` |  |
| `identifier` | `str` |  |
| `fn` | `Callable[..., None]` |  |

#### `AuditRecorder`  ·  _class_  ·  [L188]

Mirror SQLAlchemy commits into a hierarchical on-disk audit tree.

**Instance attributes**: `_run_dir`, `_task_center_run_id`, `_bus`, `_primary_roles`, `_scenario_name`, `_instance_id`, `_sandbox_id`, `_coding_plan_mode_active`, `_workflow_dir`, `_iteration_dir`, `_attempt_dir`, `_task_dir`, `_task_recorder`, `_agent_run_to_task`, `_workflow_seq_counter`, `_iteration_seq_counter`, `_attempt_seq_counter`, `_role_seq_counter`, `_listeners`, `_metrics`, `_metrics_unsub`, `_sandbox_events_unsub`, `_started_ts`, `_finished_ts`, `_status`, `_daemon_audit_puller`, `_sandbox_events_sink`, `_daemon_audit_boot_epoch_id`, `_final_daemon_audit_puller_stats`

<details><summary>Methods (28)</summary>

`__init__`, `run_dir`, `metrics`, `message_recorder_for_task`, `bind_task_center_run_id`, `message_recorder_for_agent_run`, `start`, `_maybe_auto_start_daemon_audit_puller`, `attach_daemon_audit_puller`, `daemon_audit_puller_stats`, `final_daemon_audit_puller_stats`, `stop_daemon_audit_puller`, `aclose`, `dispose`, `_dispose_sync`, `_register`, `_handle_workflow`, `_handle_iteration`, `_handle_attempt`, `_handle_task`, `_handle_agent_run`, `_record_sandbox_event`, `_ensure_workflow_dir`, `_ensure_iteration_dir`, `_ensure_attempt_dir`, `_resolve_task_dir`, `_display_role`, `_write_run_json`

</details>

---

## `task_center_runner/audit/sandbox_events_sink.py`

#### `RotatingJsonlSink`  ·  _class_  ·  [L38]

Append-only JSONL sink with size-based rotation and gzip compression.

**Instance attributes**: `_path`, `_rotation_bytes`, `_retention_files`, `_lock`

<details><summary>Methods (7)</summary>

`__init__`, `append_event`, `_maybe_rotate_locked`, `_rotate_locked`, `_next_rotation_index`, `_existing_rotations`, `_enforce_retention_locked`

</details>

---

## `task_center_runner/benchmarks/sweevo/_snapshot.py`

#### `SnapshotNotRegisteredError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L20]

Raised when a SWE-EVO snapshot is missing.

---

## `task_center_runner/benchmarks/sweevo/eval.py`

#### `_TestSetOutcome`  ·  _dataclass_  ·  decorators: `@dataclasses.dataclass(frozen=True)`  ·  [L41]

Result record summarizing how many tests passed, were runnable, and were dropped as unfindable during a SWE-Evo eval.

**Fields**

| name | type | default |
|------|------|---------|
| `passed` | `int` |  |
| `runnable_total` | `int` |  |
| `dropped_unfindable` | `int` | `0` |

#### `SweevoLifecycle`  ·  _class_  ·  [L431]

``LifecycleHooks`` implementation for SWE-EVO benchmark runs.

**Instance attributes**: `_instance`, `_repo_dir`, `_aggregate_jsonl_path`, `_aborted_reason`

<details><summary>Methods (6)</summary>

`__init__`, `before_run`, `on_event`, `on_aborted`, `after_run`, `_append_aggregate_line`

</details>

---

## `task_center_runner/benchmarks/sweevo/models.py`

#### `SWEEvoInstance`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L32]

A single SWE-EVO benchmark instance.

**Fields**

| name | type | default |
|------|------|---------|
| `instance_id` | `str` |  |
| `repo` | `str` |  |
| `base_commit` | `str` |  |
| `problem_statement` | `str` |  |
| `patch` | `str` |  |
| `fail_to_pass` | `list[str]` |  |
| `pass_to_pass` | `list[str]` |  |
| `docker_image` | `str` |  |
| `test_cmds` | `str` |  |
| `environment_setup_commit` | `str` |  |
| `test_patch` | `str` | `''` |
| `start_version` | `str` | `''` |
| `end_version` | `str` | `''` |
| `instance_id_swe` | `str` | `''` |
| `pr_description` | `str` | `''` |

#### `SWEEvoResult`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L53]

Result of running a SWE-EVO instance through EphemeralOS.

**Fields**

| name | type | default |
|------|------|---------|
| `plan_id` | `str` |  |
| `instance_id` | `str` |  |
| `status` | `str` | `'pending'` |
| `agent_patch` | `str` | `''` |
| `resolved` | `bool` | `False` |
| `fix_rate` | `float` | `0.0` |
| `fail_to_pass_passed` | `int` | `0` |
| `fail_to_pass_total` | `int` | `0` |
| `pass_to_pass_broken` | `int` | `0` |
| `pass_to_pass_total` | `int` | `0` |
| `duration_s` | `float` | `0.0` |
| `task_count` | `int` | `0` |
| `tasks_completed` | `int` | `0` |
| `tasks_failed` | `int` | `0` |
| `error` | `str` | `''` |
| `task_summaries` | `dict[str, str]` | `field(default_factory=dict)` |

#### `PreContext`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L75]

Inputs assembled by ``setup.preflight`` and threaded into ``provision_sandbox``.

**Fields**

| name | type | default |
|------|------|---------|
| `instance` | `'SWEEvoInstance'` |  |
| `repo_dir` | `str` |  |
| `snapshot_name` | `str` |  |
| `goal` | `str` |  |
| `audit_dir` | `Path` |  |
| `max_duration_s` | `float` |  |

---

## `task_center_runner/benchmarks/sweevo/run.py`

#### `SweevoProvisioner`  ·  _class_  ·  [L25]

Verify-only provisioner — the caller owns the container lifecycle.

**Instance attributes**: `_instance`, `_sandbox_id`, `_repo_dir`, `_install_lsp`

<details><summary>Methods (3)</summary>

`__init__`, `provision`, `release`

</details>

---

## `task_center_runner/core/config.py`

#### `RunConfig`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L32]

Engine input. ``runner_factory`` returning ``None`` selects the real-LLM path.

**Fields**

| name | type | default |
|------|------|---------|
| `entry_prompt` | `str` |  |
| `repo_dir` | `str` |  |
| `sandbox` | `SandboxProvisioner` |  |
| `runner_factory` | `Callable[['RunContext'], AttemptAgentRunner \| None]` |  |
| `lifecycle` | `LifecycleHooks` | `field(default_factory=NoopLifecycle)` |
| `bootstrap` | `Callable[[], None] \| None` | `None` |
| `stores` | `'TaskCenterStoreBundle \| None'` | `None` |
| `audit_dir` | `Path` | `Path('.sweevo_runs')` |
| `run_label` | `str` | `'task_center_runner'` |
| `run_dir_factory` | `Callable[[Path, 'RunContext'], Path] \| None` | `None` |
| `sandbox_provisioner_factory` | `Callable[[], 'TaskCenterSandboxProvisioner'] \| None` | `None` |
| `instance_id` | `str` | `''` |
| `max_duration_s` | `float \| None` | `None` |
| `extras` | `Mapping[str, Any]` | `field(default_factory=dict)` |

#### `RunContext`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L52]

Per-run handle passed to lifecycle hooks + the sandbox provisioner.

**Fields**

| name | type | default |
|------|------|---------|
| `config` | `RunConfig` |  |
| `bundle` | `'TaskCenterStoreBundle'` |  |
| `bus` | `'AuditEventBus'` |  |

---

## `task_center_runner/core/lifecycle.py`

#### `LifecycleHooks`  ·  _protocol_  ·  bases: `Protocol`  ·  [L26]

Per-mode hook surface assembled by adapters (scenario, sweevo, ...).

<details><summary>Methods (4)</summary>

`before_run`, `on_event`, `after_run`, `on_aborted`

</details>

#### `NoopLifecycle`  ·  _class_  ·  [L38]

Default ``LifecycleHooks`` implementation that does nothing.

<details><summary>Methods (4)</summary>

`before_run`, `on_event`, `after_run`, `on_aborted`

</details>

---

## `task_center_runner/core/real_agent_run.py`

#### `RealAgentRunReport`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L38]

Compact result handed back to the CLI / pytest entrypoints.

**Fields**

| name | type | default |
|------|------|---------|
| `instance_id` | `str` |  |
| `task_center_run_id` | `str` |  |
| `sandbox_id` | `str` |  |
| `run_dir` | `Path` |  |
| `task_center_status` | `str \| None` |  |
| `sweevo_result` | `SWEEvoResult` |  |
| `aborted_by_timeout` | `bool` | `False` |
| `performance_report_task` | `asyncio.Task[Path] \| None` | `None` |

---

## `task_center_runner/core/report.py`

#### `PipelineReport`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L24]

Result returned by ``run_pipeline``; lifecycle hooks may mutate extras.

**Fields**

| name | type | default |
|------|------|---------|
| `status` | `Literal['completed', 'aborted']` |  |
| `task_center_run_id` | `str` |  |
| `request_id` | `str` |  |
| `sandbox_id` | `str` |  |
| `instance_id` | `str` |  |
| `run_dir` | `Path` |  |
| `task_center_status` | `str \| None` |  |
| `duration_s` | `float` |  |
| `task_count` | `int` |  |
| `tasks_completed` | `int` |  |
| `tasks_failed` | `int` |  |
| `metrics` | `Mapping[str, Any]` |  |
| `aborted_by_timeout` | `bool` |  |
| `lifecycle_extras` | `dict[str, Any]` | `field(default_factory=dict)` |
| `performance_report_task` | `asyncio.Task[Path] \| None` | `None` |

---

## `task_center_runner/core/runner.py`

#### `RunReport`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L58]

Result of one :func:`run_scenario` invocation.

**Fields**

| name | type | default |
|------|------|---------|
| `scenario_name` | `str` |  |
| `task_center_run_id` | `str` |  |
| `request_id` | `str` |  |
| `sandbox_id` | `str` |  |
| `instance_id` | `str` |  |
| `run_dir` | `Path` |  |
| `task_center_status` | `str \| None` |  |
| `duration_s` | `float` |  |
| `events` | `list[Event]` | `field(default_factory=list)` |
| `seen_event_types` | `list[EventType]` | `field(default_factory=list)` |
| `hook_results` | `list[HookResult]` | `field(default_factory=list)` |
| `mutable_state_flags` | `dict[str, Any]` | `field(default_factory=dict)` |
| `launches` | `list[LaunchRecord]` | `field(default_factory=list)` |
| `tool_calls` | `list[ToolCallRecord]` | `field(default_factory=list)` |
| `prompt_inspections` | `list[PromptInspection]` | `field(default_factory=list)` |
| `sandbox_checks` | `list[SandboxCheck]` | `field(default_factory=list)` |
| `metrics` | `dict[str, Any]` | `field(default_factory=dict)` |
| `graph_summary` | `dict[str, Any]` | `field(default_factory=dict)` |
| `entry_prompt_sha256` | `str` | `''` |
| `entry_prompt_length` | `int` | `0` |
| `requirement_ledger` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `package_plan` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `matrix_plan` | `list[dict[str, Any]]` | `field(default_factory=list)` |
| `performance_report_task` | `asyncio.Task[Path] \| None` | `None` |

<details><summary>Methods (2)</summary>

`passed_prompt_inspections`, `passed_sandbox_checks`

</details>

---

## `task_center_runner/core/sandbox.py`

#### `SandboxLease`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L20]

A handle to a provisioned sandbox: an id plus opaque metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `sandbox_id` | `str` |  |
| `metadata` | `Mapping[str, Any]` | `field(default_factory=dict)` |

#### `SandboxProvisioner`  ·  _protocol_  ·  bases: `Protocol`  ·  [L27]

Provision and release a sandbox for the lifetime of a single run.

<details><summary>Methods (2)</summary>

`provision`, `release`

</details>

#### `AttachExisting`  ·  _class_  ·  [L35]

Attach to a pre-existing sandbox; ``release()`` is a no-op.

**Instance attributes**: `_lease`

<details><summary>Methods (3)</summary>

`__init__`, `provision`, `release`

</details>

---

## `task_center_runner/core/stores.py`

#### `TaskCenterStoreBundle`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L30]

Bundle of TaskCenter stores bound to an isolated test database.

**Fields**

| name | type | default |
|------|------|---------|
| `engine` | `Engine` |  |
| `schema` | `str` |  |
| `session_factory` | `sessionmaker[Session]` |  |
| `task_store` | `TaskCenterStore` |  |
| `workflow_store` | `WorkflowStore` |  |
| `iteration_store` | `IterationStore` |  |
| `attempt_store` | `AttemptStore` |  |
| `context_packet_store` | `ContextPacketStore` |  |
| `owns_engine` | `bool` | `False` |
| `cleanup_paths` | `tuple[Path, ...]` | `()` |

<details><summary>Methods (1)</summary>

`close`

</details>

---

## `task_center_runner/environments/sweevo_image/fixtures.py`

#### `_SweevoSessionLock`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L39]

File-path-based lock guarding concurrent access to a shared Sweevo image session.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `Path` |  |

---

## `task_center_runner/hooks/registry.py`

#### `HookResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L18]

One firing of a hook.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `asserted` | `bool` | `False` |
| `failed_reason` | `str \| None` | `None` |
| `extras` | `dict[str, Any]` | `field(default_factory=dict)` |

#### `MutableMockState`  ·  _class_  ·  [L27]

Cross-hook mutable state.

**Class variables**: `__slots__`

**Instance attributes**: `seen_events`, `flags`, `_failures`, `_next_planner_response`, `_next_advisor_verdict`

<details><summary>Methods (7)</summary>

`__init__`, `inject_failure`, `replace_next_planner_response`, `consume_failure`, `consume_next_planner_response`, `set_next_advisor_verdict`, `consume_advisor_verdict`

</details>

#### `Hook`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L109]

A registered hook.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `event` | `EventType` |  |
| `when` | `Literal['pre', 'post']` |  |
| `fn` | `Callable[[Event, MutableMockState], HookResult]` |  |

#### `HookSet`  ·  _class_  ·  [L118]

Insertion-ordered registry of Hooks.

**Instance attributes**: `_hooks`

<details><summary>Methods (4)</summary>

`__init__`, `register`, `fire`, `__len__`

</details>

---

## `task_center_runner/scenarios/base.py`

#### `ToolCallSpec`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L19]

Description of an agent submission tool call.

**Fields**

| name | type | default |
|------|------|---------|
| `tool` | `Any` |  |
| `args` | `dict[str, Any]` |  |

#### `ScenarioContext`  ·  _dataclass_  ·  decorators: `@dataclass(slots=True)`  ·  [L27]

Live state visible to a scenario at a decision point.

**Fields**

| name | type | default |
|------|------|---------|
| `attempt` | `Any` |  |
| `iteration` | `Any` |  |
| `workflow` | `Any` |  |
| `prompt` | `str` |  |
| `metadata` | `Any` |  |
| `audit_recorder` | `Any` |  |
| `mutable_state` | `Any` |  |
| `task_id` | `str \| None` | `None` |
| `agent_name` | `str \| None` | `None` |
| `context_message` | `str \| None` | `None` |
| `graph_summary` | `dict[str, Any] \| None` | `None` |
| `requirement_ledger` | `Any` | `None` |
| `package_plan` | `Any` | `None` |
| `matrix_plan` | `Any` | `None` |

#### `Scenario`  ·  _protocol_  ·  bases: `Protocol`  ·  decorators: `@runtime_checkable`  ·  [L47]

A scenario that drives one mock-agent run end-to-end.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `expected_event_sequence` | `tuple[EventType, ...]` |  |

<details><summary>Methods (6)</summary>

`planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`, `hooks`

</details>

#### `ScenarioBase`  ·  _class_  ·  [L66]

Default implementation of the Scenario protocol.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` | `''` |
| `expected_event_sequence` | `tuple[EventType, ...]` | `()` |

<details><summary>Methods (6)</summary>

`planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`, `hooks`

</details>

---

## `task_center_runner/scenarios/capacity/full_system_capacity_matrix.py`

#### `FullSystemCapacityMatrix`  ·  _class_  ·  bases: `FullStackAdversarial`  ·  [L17]

Composite capacity run across TaskCenter, sandbox, plugins, and audit.

**Class variables**: `name = 'capacity.full_system_capacity_matrix'`

<details><summary>Methods (2)</summary>

`executor_actions`, `_final_plan`

</details>

---

## `task_center_runner/scenarios/capacity/pack_catalog.py`

#### `CapacityPackSpec`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L16]

One scenario-pack row and its current implementation anchor.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `pack` | `str` |  |
| `owner` | `str` |  |
| `tier` | `str` |  |
| `registry_name` | `str \| None` | `None` |
| `test_path` | `str \| None` | `None` |
| `superseded_by` | `str \| None` | `None` |

<details><summary>Methods (1)</summary>

`implementation_anchor`

</details>

---

## `task_center_runner/scenarios/correctness_testing.py`

#### `CorrectnessTesting`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L98]

Single composite scenario validating framework end-to-end.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_DEFERS_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.SANDBOX_BATCH_EDIT_APPLIED, EventType.SANDBOX_CONFLICT_DETECTED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'correctness_testing'`

<details><summary>Methods (4)</summary>

`planner_response`, `executor_actions`, `evaluator_response`, `hooks`

</details>

---

## `task_center_runner/scenarios/full_case_user_input.py`

#### `FullCaseUserInput`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L38]

Exercise user-input parsing, dynamic DAGs, verifiers, and recursion.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.VERIFIER_INVOKED, EventType.VERIFIER_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_DEFERS_GOAL_PLAN, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'full_case_user_input'`

**Instance attributes**: `_user_input_plan`, `_entry_prompt`, `_recursive_package_id`

<details><summary>Methods (15)</summary>

`__init__`, `requirement_ledger`, `package_plan`, `planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`, `hooks`, `_entry_origin_planner_response`, `_recursive_planner_response`, `_implementation_plan`, `_final_reconciliation_plan`, `_ensure_user_input_plan`, `_should_fail_verifier`

</details>

---

## `task_center_runner/scenarios/full_stack_adversarial.py`

#### `FullStackCell`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L42]

One named matrix cell emitted to the full-stack metrics artifact.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `subsystem` | `str` |  |
| `tool_names` | `tuple[str, ...]` |  |
| `package_id` | `str \| None` | `None` |
| `route` | `str` | `'gated'` |

#### `FullStackAdversarial`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L52]

Drive TaskCenter, sandbox, OCC, layer-stack, LSP, and recursion.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EVALUATOR_FAILURE, EventType.PLANNER_DEFERS_GOAL_PLAN, EventType.VERIFIER_FAILURE, EventType.RECURSIVE_WORKFLOW_REQUESTED, EventType.RECURSIVE_WORKFLOW_COMPLETED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'full_stack_adversarial'`

**Instance attributes**: `_user_input_plan`, `_entry_prompt`, `_forced_failure_seen`, `_recursive_package_id`, `_matrix_cells`

<details><summary>Methods (18)</summary>

`__init__`, `requirement_ledger`, `package_plan`, `matrix_plan`, `planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`, `hooks`, `_entry_origin_planner_response`, `_recursive_planner_response`, `_subsystem_wave_plan`, `_retry_deferred_plan`, `_final_plan`, `_ensure_user_input_plan`, `_ensure_matrix_cells`, `_should_fail_verifier`

</details>

---

## `task_center_runner/scenarios/lifecycle.py`

#### `ScenarioLifecycle`  ·  _class_  ·  [L31]

``LifecycleHooks`` implementation for the mock-scenario mode.

**Instance attributes**: `_scenario`, `_hook_set`, `_mutable_state`, `_hook_results`, `_captured_events`, `_launches`, `_tool_calls`, `_prompt_inspections`, `_sandbox_checks`

<details><summary>Methods (11)</summary>

`__init__`, `captured_events`, `hook_results`, `launches`, `tool_calls`, `prompt_inspections`, `sandbox_checks`, `before_run`, `on_event`, `after_run`, `on_aborted`

</details>

---

## `task_center_runner/scenarios/pipeline/attempt_budget_exhausted.py`

#### `AttemptBudgetExhausted`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L51]

Every attempt fails — budget exhaustion closes the workflow failed.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_FAILURE)` |

**Class variables**: `name = 'pipeline.attempt_budget_exhausted'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/attempt_retry_evaluator_failure.py`

#### `AttemptRetryEvaluatorFailure`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L28]

Attempt 1 fails (evaluator), attempt 2 passes — same iteration.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.attempt_retry_evaluator_failure'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/attempt_retry_generator_failure.py`

#### `AttemptRetryGeneratorFailure`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L33]

Attempt 1 generator fails, attempt 2 succeeds.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.attempt_retry_generator_failure'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/attempt_retry_planner_failure.py`

#### `AttemptRetryPlannerFailure`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L27]

Attempt 1 planner fails validation, attempt 2 emits a valid plan.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.attempt_retry_planner_failure'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/deferred_parent_planner_terminal_routing.py`

#### `DeferredParentPlannerTerminalRouting`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L89]

Child workflow from a partial parent gets the restricted planner terminals.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_DEFERS_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.RECURSIVE_WORKFLOW_REQUESTED, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_SUCCESS, EventType.VERIFIER_INVOKED, EventType.RECURSIVE_WORKFLOW_COMPLETED, EventType.VERIFIER_SUCCESS, EventType.EVALUATOR_SUCCESS, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.deferred_parent_planner_terminal_routing'`

<details><summary>Methods (5)</summary>

`planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`

</details>

---

## `task_center_runner/scenarios/pipeline/dependency_blocked_descendants.py`

#### `DependencyBlockedDescendants`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L39]

Blocked root leaves descendants pending until the attempt fails.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_FAILURE)` |

**Class variables**: `name = 'pipeline.dependency_blocked_descendants'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/dependency_dag_diamond.py`

#### `DependencyDagDiamond`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L36]

Diamond readiness and dependency-summary rendering scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.dependency_dag_diamond'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/dependency_dag_mixed.py`

#### `DependencyDagMixed`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L64]

Mixed serial + parallel DAG; task dispatcher honours fan-in semantics.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.dependency_dag_mixed'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/dependency_dag_parallel.py`

#### `DependencyDagParallel`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L36]

Focused fan-in scenario: a, b, c -> d.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.dependency_dag_parallel'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/dependency_dag_serial.py`

#### `DependencyDagSerial`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L45]

Serial DAG; assert executor invocation order matches dependency order.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.dependency_dag_serial'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/generator_failure_quiescence.py`

#### `GeneratorFailureQuiescence`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L73]

Sibling quiescence on failure → retry passes the same plan cleanly.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.generator_failure_quiescence'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/initial_messages_capture.py`

#### `InitialMessagesCapture`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L81]

Continuation + attempt retry, single executor task per attempt.

**Fields**

| name | type | default |
|------|------|---------|
| `call_helpers_in_executor` | `bool` | `False` |
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_FAILURE, EventType.PLANNER_INVOKED, EventType.PLANNER_DEFERS_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.initial_messages_capture'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/initial_workflow.py`

#### `InitialWorkflow`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L25]

Single workflow, single iteration, single attempt — happy path.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.initial_workflow'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/iterative_deferral.py`

#### `IterativeDeferral`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L36]

Iteration 1 partial plan → iteration 2 full plan; both pass.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_DEFERS_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS, EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.iterative_deferral'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/pipeline/nested_workflow.py`

#### `NestedWorkflow`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L84]

Parent generator delegates to a child workflow, then reconciles.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.RECURSIVE_WORKFLOW_REQUESTED, EventType.RECURSIVE_WORKFLOW_COMPLETED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'pipeline.nested_workflow'`

<details><summary>Methods (5)</summary>

`planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`

</details>

#### `NestedWorkflowFailure`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L134]

Child workflow exhausts attempts and parent workflow fails cleanly.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.RECURSIVE_WORKFLOW_REQUESTED)` |

**Class variables**: `name = 'pipeline.nested_workflow_failure'`

<details><summary>Methods (5)</summary>

`planner_response`, `executor_actions`, `verifier_response`, `evaluator_response`, `recursive_handoff_goal`

</details>

---

## `task_center_runner/scenarios/planner_validation/cycle_in_deps.py`

#### `PlannerCycleInDeps`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L30]

Plan contains a dependency cycle.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED)` |

**Class variables**: `name = 'planner_validation.cycle_in_deps'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/planner_validation/defers_without_deferred_goal.py`

#### `PlannerDefersWithoutDeferredGoal`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L24]

submit_plan_defers_goal call omits required deferred_goal.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED)` |

**Class variables**: `name = 'planner_validation.defers_without_deferred_goal'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/planner_validation/duplicate_local_id.py`

#### `PlannerDuplicateLocalId`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L45]

Planner returns a duplicate-id plan; attempt closes planner_failed.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED)` |

**Class variables**: `name = 'planner_validation.duplicate_local_id'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/planner_validation/empty_tasks.py`

#### `PlannerEmptyTasks`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L24]

Full plan with no generator tasks.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED)` |

**Class variables**: `name = 'planner_validation.empty_tasks'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/planner_validation/unknown_agent_name.py`

#### `PlannerUnknownAgentName`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L26]

Plan references an unknown generator-capable agent.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED)` |

**Class variables**: `name = 'planner_validation.unknown_agent_name'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/planner_validation/unknown_dep.py`

#### `PlannerUnknownDep`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L30]

Plan references an unknown local dependency id.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_INVOKED)` |

**Class variables**: `name = 'planner_validation.unknown_dep'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/_fixtures/lsp_expectations.py`

#### `LspExpectation`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L14]

Expected LSP result spec for a symbol's definition, references, and hover content used to validate sandbox LSP behavior.

**Fields**

| name | type | default |
|------|------|---------|
| `symbol` | `str` |  |
| `source_path` | `str` |  |
| `source_anchor` | `str` |  |
| `definition_path` | `str` |  |
| `definition_anchor` | `str` |  |
| `min_references` | `int` |  |
| `hover_contains` | `tuple[str, ...]` |  |

---

## `task_center_runner/scenarios/sandbox/_fixtures/refactor_passes.py`

#### `RefactorEdit`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L22]

A single sentinel-comment edit applied at an anchor in a target file during a refactor scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `relative_path` | `str` |  |
| `anchor` | `str` |  |
| `sentinel` | `str` |  |

#### `LSPRefSpec`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L29]

Specifies a target file line and symbol for converting an anchor substring into an LSP cursor position.

**Fields**

| name | type | default |
|------|------|---------|
| `relative_path` | `str` |  |
| `line_index_anchor` | `str` |  |
| `symbol` | `str \| None` | `None` |

#### `RefactorPass`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L38]

Named refactor scenario bundling a target symbol with its edits and LSP reference targets.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `description` | `str` |  |
| `target_symbol` | `str` |  |
| `edits` | `tuple[RefactorEdit, ...]` |  |
| `lsp_targets` | `tuple[LSPRefSpec, ...]` |  |

---

## `task_center_runner/scenarios/sandbox/_fixtures/scheduler_demo_data.py`

#### `Patch`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L19]

Frozen dataclass describing a single search/replace edit (old text, new text, description) applied to a fixture file.

**Fields**

| name | type | default |
|------|------|---------|
| `old_text` | `str` |  |
| `new_text` | `str` |  |
| `description` | `str` |  |

#### `FixtureFile`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L26]

Frozen dataclass modeling a scenario fixture file with its final content, skeleton, and ordered edit-progression patches.

**Fields**

| name | type | default |
|------|------|---------|
| `relative_path` | `str` |  |
| `final` | `str` |  |
| `skeleton` | `str` |  |
| `patches` | `tuple[Patch, ...]` | `field(default_factory=tuple)` |
| `is_init` | `bool` | `False` |

---

## `task_center_runner/scenarios/sandbox/auto_squash_commit_resume.py`

#### `AutoSquashCommitResume`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L63]

OCC mutation critical-path probe across AUTO_SQUASH_MAX_DEPTH.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.SANDBOX_CONFLICT_DETECTED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'sandbox.auto_squash_commit_resume'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/background_shell.py`

#### `_BackgroundShellScenarioBase`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L58]

Shared planner/executor/evaluator shape across the 7 scenarios.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |
| `action_id` | `str` | `''` |
| `action_spec` | `str` | `''` |
| `summary_path_hint` | `str` | `''` |

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

#### `BackgroundShellGolden`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L98]

T1: N concurrent background launches reach ``finished`` cleanly.

**Class variables**: `name = 'sandbox.background_shell_golden'`, `action_id = 'background_shell_golden'`, `action_spec`, `summary_path_hint`

#### `BackgroundShellStop`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L113]

T2: launch background shells; cancel mid-flight; no leftover state.

**Class variables**: `name = 'sandbox.background_shell_stop'`, `action_id = 'background_shell_stop'`, `action_spec`, `summary_path_hint`

#### `BackgroundShellInterleave`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L129]

T3: 1 long background + M foreground shells, record fg p95 mount.

**Class variables**: `name = 'sandbox.background_shell_interleave'`, `action_id = 'background_shell_interleave'`, `action_spec`, `summary_path_hint`

#### `BackgroundShellExhaustion`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L144]

T5: 80 launches cancelled in unison; AC-14 post-exhaustion read budget.

**Class variables**: `name = 'sandbox.background_shell_exhaustion'`, `action_id = 'background_shell_exhaustion'`, `action_spec`, `summary_path_hint`

#### `BackgroundShellPartialWriteCancel`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L160]

T6: cancel a long ``dd`` mid-write; assert no leaked OCC publish.

**Class variables**: `name = 'sandbox.background_shell_partial_write_cancel'`, `action_id = 'background_shell_partial_write_cancel'`, `action_spec`, `summary_path_hint`

#### `BackgroundShellStopDuringMaintenance`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L175]

T7: short shell + maintenance; verify OCC consistency afterwards.

**Class variables**: `name = 'sandbox.background_shell_stop_during_maintenance'`, `action_id = 'background_shell_stop_during_maintenance'`, `action_spec`, `summary_path_hint`

#### `BackgroundShellLateCancelRace`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L190]

T8: await full completion; late cancel must not mutate the result.

**Class variables**: `name = 'sandbox.background_shell_late_cancel_race'`, `action_id = 'background_shell_late_cancel_race'`, `action_spec`, `summary_path_hint`

#### `BackgroundMixedFgBgSamePathConflict`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L205]

3.3.1: direct foreground write races a background shell publish.

**Class variables**: `name = 'sandbox.background_mixed_fg_bg_same_path_conflict'`, `action_id = 'background_mixed_fg_bg_same_path_conflict'`, `action_spec`, `summary_path_hint`

#### `BackgroundHeartbeatLossReapsOnlyStaleBg`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L222]

3.3.2: heartbeat one background invocation while another goes stale.

**Class variables**: `name = 'sandbox.background_heartbeat_loss_reaps_only_stale_bg'`, `action_id = 'background_heartbeat_loss_reaps_only_stale_bg'`, `action_spec`, `summary_path_hint`

#### `BackgroundExitIwsDrainsAgentTasks`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L239]

3.3.3: iws enter blocks default bg and iws exit drains per-agent bg.

**Class variables**: `name = 'sandbox.background_exit_iws_drains_agent_tasks'`, `action_id = 'background_exit_iws_drains_agent_tasks'`, `action_spec`, `summary_path_hint`

#### `BackgroundEngineRestartNoLeaseLeak`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L256]

3.3.4: abandoned background work is reaped before foreground recovery.

**Class variables**: `name = 'sandbox.background_engine_restart_no_lease_leak'`, `action_id = 'background_engine_restart_no_lease_leak'`, `action_spec`, `summary_path_hint`

#### `BackgroundManySmallWritesDoNotStarveDispatcher`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L272]

3.3.5: many small background writes with interleaved foreground files.

**Class variables**: `name`, `action_id = 'background_many_small_writes_do_not_starve_dispatcher'`, `action_spec`, `summary_path_hint`

#### `BackgroundMixedOpConcurrent`  ·  _class_  ·  bases: `_BackgroundShellScenarioBase`  ·  [L289]

3.3.6: heterogeneous + conflicting + disjoint concurrent background work.

**Class variables**: `name = 'sandbox.background_mixed_op_concurrent'`, `action_id = 'background_mixed_op_concurrent'`, `action_spec`, `summary_path_hint`

---

## `task_center_runner/scenarios/sandbox/complex_project_build.py`

#### `ComplexProjectBuild`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L102]

Full nightly form of the complex project-build scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `_EXPECTED_EVENT_SEQUENCE` |

**Class variables**: `name = 'sandbox.complex_project_build'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

#### `ComplexProjectBuildSmoke`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L128]

Smoke variant for pre-merge gating — same probe, smaller fixture set.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `_EXPECTED_EVENT_SEQUENCE` |

**Class variables**: `name = 'sandbox.complex_project_build_smoke'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/complex_project_build_grep_glob.py`

#### `ComplexProjectBuildGrepGlob`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L90]

Full heavy grep + glob + edit_file project-build scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `_EXPECTED_EVENT_SEQUENCE` |

**Class variables**: `name = 'sandbox.complex_project_build_grep_glob'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

#### `ComplexProjectBuildGrepGlobSmoke`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L115]

Smoke variant of the grep + glob workflow scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `_EXPECTED_EVENT_SEQUENCE` |

**Class variables**: `name = 'sandbox.complex_project_build_grep_glob_smoke'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/complex_project_build_shell_edit_lsp.py`

#### `ComplexProjectBuildShellEditLsp`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L82]

Full mixed shell-edit + semantic LSP project-build scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `_EXPECTED_EVENT_SEQUENCE` |

**Class variables**: `name = 'sandbox.complex_project_build_shell_edit_lsp'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

#### `ComplexProjectBuildShellEditLspSmoke`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L107]

Smoke variant of the mixed shell-edit + semantic LSP scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `_EXPECTED_EVENT_SEQUENCE` |

**Class variables**: `name = 'sandbox.complex_project_build_shell_edit_lsp_smoke'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/ephemeral_workspace.py`

#### `_EphemeralWorkspaceScenarioBase`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L36]

Base scenario driving planner/executor/evaluator responses for an ephemeral-workspace action against a summary path.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |
| `action_id` | `str` | `''` |
| `action_spec` | `str` | `''` |
| `summary_path_hint` | `str` | `''` |

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

#### `EphemeralWorkspaceAllVerbs`  ·  _class_  ·  bases: `_EphemeralWorkspaceScenarioBase`  ·  [L72]

Scenario exercising every ephemeral-workspace verb (write, read, edit, grep, glob, shell) with manifest and cleanup checks.

**Class variables**: `name = 'sandbox.ephemeral_workspace_all_verbs'`, `action_id = 'ephemeral_workspace_all_verbs'`, `action_spec`, `summary_path_hint`

#### `EphemeralWorkspaceConcurrentWrites`  ·  _class_  ·  bases: `_EphemeralWorkspaceScenarioBase`  ·  [L85]

Scenario launching concurrent disjoint-path writes and shell captures to assert typed/api versus overlay source tags.

**Class variables**: `name = 'sandbox.ephemeral_workspace_concurrent_writes'`, `action_id = 'ephemeral_workspace_concurrent_writes'`, `action_spec`, `summary_path_hint`

#### `EphemeralWorkspaceSamePathConflict`  ·  _class_  ·  bases: `_EphemeralWorkspaceScenarioBase`  ·  [L99]

Scenario forcing same-path writes to require typed OCC conflicts, retry after fresh reads, and verify final content.

**Class variables**: `name = 'sandbox.ephemeral_workspace_same_path_conflict'`, `action_id = 'ephemeral_workspace_same_path_conflict'`, `action_spec`, `summary_path_hint`

#### `EphemeralWorkspacePolicy`  ·  _class_  ·  bases: `_EphemeralWorkspaceScenarioBase`  ·  [L113]

Mock scenario exercising ephemeral-workspace read/write success and denied writes to protected system paths through the request pipeline.

**Class variables**: `name = 'sandbox.ephemeral_workspace_policy'`, `action_id = 'ephemeral_workspace_policy'`, `action_spec`, `summary_path_hint`

#### `EphemeralWorkspaceCancellation`  ·  _class_  ·  bases: `_EphemeralWorkspaceScenarioBase`  ·  [L126]

Mock scenario that cancels a long writing shell and verifies no partial publish plus a healthy read/write cycle.

**Class variables**: `name = 'sandbox.ephemeral_workspace_cancellation'`, `action_id = 'ephemeral_workspace_cancellation'`, `action_spec`, `summary_path_hint`

#### `EphemeralWorkspaceO1Disk`  ·  _class_  ·  bases: `_EphemeralWorkspaceScenarioBase`  ·  [L139]

Mock scenario running 100 sequential small mutations while sampling disk and asserting manifest advancement matches mutation count.

**Class variables**: `name = 'sandbox.ephemeral_workspace_o1_disk'`, `action_id = 'ephemeral_workspace_o1_disk'`, `action_spec`, `summary_path_hint`

---

## `task_center_runner/scenarios/sandbox/heavy_io_zoned_concurrent.py`

#### `HeavyIoZonedConcurrent`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L87]

Long-running zoned-IO scenario for layerstack lease + OCC merge.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'sandbox.heavy_io_zoned_concurrent'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/high_concurrency_layerstack_overlay_occ.py`

#### `HighConcurrencyLayerstackOverlayOcc`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L87]

Capacity case for concurrent layer-stack, overlay, and OCC pressure.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.SANDBOX_CONFLICT_DETECTED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'sandbox.high_concurrency_layerstack_overlay_occ'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/occ_concurrent_conflicts.py`

#### `OccConcurrentConflicts`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L52]

OCC + layer-stack + overlay + conflict round trip via sandbox_integrity.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.SANDBOX_BATCH_EDIT_APPLIED, EventType.SANDBOX_CONFLICT_DETECTED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name = 'sandbox.occ_concurrent_conflicts'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/scenarios/sandbox/plugin.py`

#### `_PluginScenarioBase`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L36]

Base class defining the planner/executor/evaluator flow and expected event sequence for sandbox plugin mock scenarios.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |
| `action_id` | `str` | `''` |
| `action_spec` | `str` | `''` |
| `summary_path_hint` | `str` | `''` |

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

#### `PluginReadOnlyLspRefresh`  ·  _class_  ·  bases: `_PluginScenarioBase`  ·  [L72]

Mock scenario proving read-only LSP services refresh warmly after an edit without per-call publish.

**Class variables**: `name = 'sandbox.plugin_read_only_lsp_refresh'`, `action_id = 'plugin_read_only_lsp_refresh'`, `action_spec`, `summary_path_hint`

#### `PluginWriteAllowedPublish`  ·  _class_  ·  bases: `_PluginScenarioBase`  ·  [L86]

Mock scenario applying a write-allowed LSP WorkspaceEdit and reading the committed change back through the sandbox API.

**Class variables**: `name = 'sandbox.plugin_write_allowed_publish'`, `action_id = 'plugin_write_allowed_publish'`, `action_spec`, `summary_path_hint`

#### `PluginIntentContract`  ·  _class_  ·  bases: `_PluginScenarioBase`  ·  [L99]

Mock scenario proving plugin intent/lifecycle gating and read-only vs write-allowed dispatch routing.

**Class variables**: `name = 'sandbox.plugin_intent_contract'`, `action_id = 'plugin_intent_contract'`, `action_spec`, `summary_path_hint`

#### `PluginIwsPolicy`  ·  _class_  ·  bases: `_PluginScenarioBase`  ·  [L113]

Mock scenario verifying plugin daemon ops are blocked inside isolated workspace and permitted by default.

**Class variables**: `name = 'sandbox.plugin_iws_policy'`, `action_id = 'plugin_iws_policy'`, `action_spec`, `summary_path_hint`

#### `PluginSetupFailure`  ·  _class_  ·  bases: `_PluginScenarioBase`  ·  [L127]

Mock scenario forcing a plugin setup/network failure then retrying to prove no stale loaded state.

**Class variables**: `name = 'sandbox.plugin_setup_failure'`, `action_id = 'plugin_setup_failure'`, `action_spec`, `summary_path_hint`

#### `PluginServiceEvict`  ·  _class_  ·  bases: `_PluginScenarioBase`  ·  [L140]

Mock scenario exercising Pyright service warm refresh, runtime eviction via digest churn, and clean restart.

**Class variables**: `name = 'sandbox.plugin_service_evict'`, `action_id = 'plugin_service_evict'`, `action_spec`, `summary_path_hint`

---

## `task_center_runner/scenarios/user_input.py`

#### `RequirementItem`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L44]

Frozen dataclass representing a single parsed user-input requirement with subsystem, risk, and weight metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `heading` | `str` |  |
| `text` | `str` |  |
| `pr_id` | `str \| None` |  |
| `subsystem` | `str` |  |
| `risk` | `str` |  |
| `weight` | `int` |  |

#### `WorkPackage`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L55]

Frozen dataclass grouping requirement items into a schedulable work unit with dependencies and risk.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` |  |
| `title` | `str` |  |
| `subsystem` | `str` |  |
| `item_ids` | `tuple[str, ...]` |  |
| `weight` | `int` |  |
| `risk` | `str` |  |
| `deps` | `tuple[str, ...]` | `()` |
| `recursive_candidate` | `bool` | `False` |

#### `UserInputPlan`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L67]

Parsed plan derived from a user prompt holding inspected text, extracted requirements, and built work packages.

**Fields**

| name | type | default |
|------|------|---------|
| `inspected_text` | `str` |  |
| `requirements` | `tuple[RequirementItem, ...]` |  |
| `packages` | `tuple[WorkPackage, ...]` |  |

---

## `task_center_runner/tests/mock/_focused_scenario_contracts.py`

#### `FocusedScenarioCase`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L15]

Test expectation spec asserting a focused mock scenario's status, event counts, and graph shape.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `expected_status` | `str` | `'done'` |
| `min_event_counts` | `Mapping[EventType, int]` | `field(default_factory=dict)` |
| `absent_events` | `Sequence[EventType]` | `()` |
| `workflow_status` | `str` | `'succeeded'` |
| `iteration_count` | `int \| None` | `1` |
| `attempt_count` | `int \| None` | `None` |

---

## `task_center_runner/tests/mock/_project_build_contracts.py`

#### `ComplexBuildContract`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L83]

Test contract defining minimum tool-call, sandbox-event, LSP, API, and test thresholds for complex build scenarios.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_call_floor` | `int` |  |
| `required_sandbox_events` | `tuple[EventType, ...]` |  |
| `require_squash_events` | `bool` |  |
| `lsp_floor` | `int` |  |
| `api_read_floor` | `int` |  |
| `api_edit_floor` | `int` |  |
| `api_shell_floor` | `int` |  |
| `require_layer_squash_metrics` | `bool` |  |
| `junit_test_floor` | `int` |  |

#### `ShellEditLspContract`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L96]

Test contract defining edit, shell-ratio, LSP, and diagnostic floors for shell-edit-LSP scenarios.

**Fields**

| name | type | default |
|------|------|---------|
| `logical_edit_floor` | `int` |  |
| `shell_ratio_tolerance` | `float` |  |
| `lsp_floor` | `int` |  |
| `total_lsp_floor` | `int` |  |
| `diagnostic_checks_floor` | `int` |  |

#### `GrepGlobContract`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L105]

Test contract defining tool-call, grep, glob, and edit floors for a named grep/glob scenario.

**Fields**

| name | type | default |
|------|------|---------|
| `scenario_name` | `str` |  |
| `tool_call_floor` | `int` |  |
| `grep_floor` | `int` |  |
| `glob_floor` | `int` |  |
| `edit_floor` | `int` |  |

---

## `task_center_runner/tests/mock/contracts/test_scenario_event_source_spike.py`

#### `_UnusedClient`  ·  _class_  ·  [L50]

``event_source`` short-circuits the loop's provider call, so this client

<details><summary>Methods (1)</summary>

`aclose`

</details>

---

## `task_center_runner/tests/mock/contracts/test_scenario_loop_runner_planner_submit.py`

#### `_PlannerSubmitProof`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L31]

Planner closes the workflow with one trivial executor task; evaluator passes.

**Class variables**: `name = 'planner_submit_proof'`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>

---

## `task_center_runner/tests/mock/sandbox/isolated_workspace/_iws_fixtures.py`

#### `SentinelFile`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L17]

A file published into the default layer via the peer flow.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `body` | `str` |  |

---

## `task_center_runner/tests/mock/sandbox/isolated_workspace/_iws_invariants.py`

#### `LatencyBudget`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L216]

HYBRID latency assertion — ratio-to-baseline AND absolute ceiling.

**Fields**

| name | type | default |
|------|------|---------|
| `baseline_ms` | `dict[str, float]` |  |
| `budget` | `dict[str, Any] \| None` |  |
| `ratio_low` | `float` | `0.3` |
| `ratio_high` | `float` | `3.0` |
| `absolute_p95_slack` | `float` | `1.5` |

<details><summary>Methods (3)</summary>

`from_paths`, `has_baseline_for`, `assert_stable_and_within_budget`

</details>

---

## `task_center_runner/tests/mock/sandbox/isolated_workspace/pre_flight/test_phase_timer_invariants.py`

#### `_FakeClock`  ·  _class_  ·  [L25]

Test double providing a manually-advanced callable clock for deterministic phase-timer timing assertions.

**Instance attributes**: `t`

<details><summary>Methods (2)</summary>

`__init__`, `__call__`

</details>

---

## `task_center_runner/tests/mock/sandbox/project_build/test_complex_project_build_fixtures.py`

#### `_ProbeCtx`  ·  _class_  ·  [L53]

Test stub probe context that records published mock records and sandbox checks for assertions.

**Instance attributes**: `sandbox_checks`, `mock_records`

<details><summary>Methods (2)</summary>

`__init__`, `publish_mock_record`

</details>

---

## `task_center_runner/tests/mock/sandbox/project_build/test_project_build_shell_edit_lsp_three_parallel_agents.py`

#### `ComplexProjectBuildShellEditLspThreeParallelAgents`  ·  _class_  ·  bases: `ScenarioBase`  ·  [L45]

Three dependency-free executor tasks inside one TaskCenter run.

**Fields**

| name | type | default |
|------|------|---------|
| `expected_event_sequence` | `tuple[EventType, ...]` | `(EventType.PLANNER_INVOKED, EventType.PLANNER_COMPLETES_GOAL_PLAN, EventType.EXECUTOR_INVOKED, EventType.EXECUTOR_SUCCESS, EventType.EVALUATOR_INVOKED, EventType.EVALUATOR_SUCCESS)` |

**Class variables**: `name`

<details><summary>Methods (3)</summary>

`planner_response`, `executor_actions`, `evaluator_response`

</details>


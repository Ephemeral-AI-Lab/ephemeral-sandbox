---
title: "Task Center Pipeline"
tags: ["task-center", "mission", "episode", "attempt", "planner", "generator", "evaluator", "submission", "lifecycle", "live-e2e", "see-also"]
created: 2026-05-10T11:26:29.073Z
updated: 2026-05-10T11:58:10.634Z
sources: []
links: ["live-e2e-testing-framework-design.md", "engine-query-loop-llm-seam.md", "context-engine-recipes.md", "sandbox-subsystem.md"]
category: architecture
confidence: medium
schemaVersion: 1
---

# Task Center Pipeline

_Source: explore agent draft, 2026-05-10. See `.omc/wiki-draft/task-center.md`._

## Hierarchy

The pipeline is a four-level tree: Mission → Episode → Attempt → Tasks.

- **Mission** (`task_center/mission/mission.py:19`) — a delegated goal. Holds an ordered Episode id list, `task_center_run_id`, and `requested_by_task_id` (the generator task that called `request_mission_solution`, or the entry task). Statuses: `open`, `succeeded`, `failed`, `cancelled`.
- **Episode** (`task_center/episode/episode.py:22`) — one planning attempt window inside a Mission. Has `attempt_budget` (default `2` from `HarnessLifecycleConfig`, `task_center/config.py:9`), ordered Attempt id list, `creation_reason` (`initial` | `partial_continuation`), and `continuation_goal` (non-null when the passing attempt submitted a partial plan).
- **Attempt** (`task_center/attempt/state.py:30`) — one planner→generator-DAG→evaluator execution. Tracks `stage` (`planning` → `generating` → `evaluating` → `closed`), `status` (`running` | `passed` | `failed`), `fail_reason`, `continuation_goal`.
- **Tasks** — per-role harness rows in `TaskCenterStore`. Roles: `planner`, `generator`, `evaluator` (`task_center/task/models.py:10`). Task ids deterministic: `{attempt_id}:planner`, `{attempt_id}:gen:{local_id}`, `{attempt_id}:evaluator` (`task_center/task/ids.py`).

The entry executor is **not** a Mission. It is a top-level task with role `generator`, `task_center_attempt_id=None`, `agent_name="entry_executor"`.

## Entry Executor

`start_task_center_entry_run` (`task_center/entry/coordinator.py:71`) — sole public entry point.

```python
def start_task_center_entry_run(
    *,
    config: RuntimeConfig,
    prompt: str,
    sandbox_id: str | None,
    on_agent_event: AgentStreamEmitter | None,
    task_store: TaskCenterStore,
    mission_store: MissionStore,
    episode_store: EpisodeStore,
    attempt_store: AttemptStore,
    runner: AttemptAgentRunner | None = None,         # seam #1
    context_packet_store: ContextPacketStore | None = None,
    sandbox_bridge: TaskCenterSandboxBridge | None = None,
) -> TaskCenterEntryHandle
```

`TaskCenterEntryCoordinator.start()` (`coordinator.py:131`):
1. `TaskCenterSandboxBridge.prepare_for_run`
2. Writes run row + entry task row (role=`GENERATOR`, status=`RUNNING`, `task_center_attempt_id=None`)
3. Creates `EntryTaskController` — single owner of entry-task transitions (`controller.py:20`)
4. Builds `AttemptRuntime` wiring stores + launcher + registries
5. `EphemeralAttemptAgentLauncher.launch(AgentLaunch)` schedules an asyncio Task
6. Returns `TaskCenterEntryHandle` (contains `launcher` for `wait_for_idle()`)

`EntryTaskController` owns: `apply_executor_success`, `apply_executor_failure`, `apply_run_exhausted`, `apply_mission_close_report`, `mark_waiting_mission`, `restore_running_after_failed_mission_start` (`controller.py:29-182`).

## Roles

| Role | Agent name | What it does | Terminal submission tool |
|---|---|---|---|
| `entry_executor` | `entry_executor` | Top-level user-request agent; either completes directly or delegates via `request_mission_solution`. Not a Mission. | `submit_execution_success` / `submit_execution_failure` |
| `planner` | `planner` | Plans one Attempt: emits `PlannerSubmission` encoding generator DAG, `task_specification`, `evaluation_criteria`, optional `continuation_goal`. | `submit_full_plan` / `submit_partial_plan` |
| `generator` (executor) | agent-specific | Executes one DAG leaf task. Optionally calls `request_mission_solution` to delegate to a child Mission. | `submit_execution_success` / `submit_execution_failure` / `request_mission_solution` |
| `generator` (verifier) | agent-specific | Verifies work produced by an executor. | `submit_verification_success` / `submit_verification_failure` |
| `evaluator` | `evaluator` | Evaluates the full DAG outcome; pass closes the Attempt. | `submit_evaluation_success` / `submit_evaluation_failure` |

## Terminal submission events drive the workflow

Each tool calls into `AttemptOrchestrator` (or `EntryTaskController` for entry-mode), driving the state machine. Submission tools resolve context via `tools/submission/context/{attempt.py,executor.py}`.

| Tool | Handler | State transition |
|---|---|---|
| `submit_full_plan` | `apply_plan_submission(kind="full")` | Planner→DONE; stage→`generating`; generator rows PENDING; dispatcher launches ready |
| `submit_partial_plan` | `apply_plan_submission(kind="partial", continuation_goal=…)` | Same + records `continuation_goal` |
| `submit_execution_success` | `apply_generator_submission(outcome="success")` | Gen→DONE; dispatcher checks quiescence → spawns evaluator if all done |
| `submit_execution_failure` | `apply_generator_submission(outcome="failure")` | Gen→FAILED; descendants BLOCKED → attempt closes FAILED/`generator_failed` |
| `submit_verification_*` | Same `apply_generator_submission` path | As executor success/failure |
| `submit_evaluation_success` | `apply_evaluator_submission(outcome="success")` | Eval→DONE; attempt closes PASSED; `EpisodeManager.handle_attempt_closed` emits `TerminalSuccess` or `SuccessContinue` |
| `submit_evaluation_failure` | `apply_evaluator_submission(outcome="failure")` | Eval→FAILED; attempt closes FAILED/`evaluator_failed` |
| `request_mission_solution` | `MissionStarter.start(goal=…)` | Creates child Mission+Episode+Attempt; parks parent gen task in `WAITING_COMPLEX_TASK`; terminal so parent agent run ends |

The dispatcher (`task_center/attempt/dispatcher.py:64`) is called after every submission via `AttemptOrchestrator.dispatch_ready_work()`:
- `generating`: launches PENDING tasks whose `needs[]` are all DONE.
- Quiescence: any FAILED/BLOCKED → close attempt FAILED; all DONE → spawn evaluator.
- `evaluating`: evaluator DONE → PASSED; FAILED → FAILED.

## Attempt retry on failure

`EpisodeManager.handle_attempt_closed` (`episode/manager.py:122`) is the closed-attempt callback wired into every `AttemptOrchestrator`.

On `AttemptStatus.FAILED`:
1. Checks `episode.has_budget_remaining` (`episode.py:58`: `attempt_count < attempt_budget`).
2. Budget remains: `create_next_attempt(previous_attempt_id=…)` → `_insert_attempt` + `_start_orchestrator_if_configured` → new `AttemptOrchestrator.start()` → new planner task.
3. Budget exhausted: `_close_episode_failed` → `AttemptPlanFailed(failure_summary, attempted_plan_history)`.

`AttemptFailReason` (`attempt/state.py:23`):
- `planner_failed` — planner submitted failure or run exhausted
- `generator_failed` — any generator terminal-failed
- `evaluator_failed` — evaluator terminal-failed
- `startup_failed` — orchestrator/launcher raised before agent started

The launcher synthesises exhaustion submissions when an agent run ends without terminal: `EphemeralAttemptAgentLauncher._report_unfinished_running_task` (`agent_launch/launcher.py:154`) calls `apply_planner_failure` / `apply_generator_submission(outcome="failure")` / `apply_evaluator_submission(outcome="failure")` per role.

## Episodic continuation

`EpisodeCreationReason` (`episode/episode.py:17`):
- `INITIAL` — first Episode of a Mission.
- `PARTIAL_CONTINUATION` — created when previous Episode's passing Attempt had `continuation_goal` set.

Flow on `submit_partial_plan`:
1. `apply_plan_submission` records `continuation_goal` via `attempt_store.set_plan_contract` (`orchestrator.py:284`).
2. Attempt stages through generating → evaluating → PASSED.
3. `EpisodeManager._close_episode_passed` writes `continuation_goal` onto episode row (`manager.py:189`), emits `SuccessContinue(goal=attempt.continuation_goal)` (`manager.py:204`).
4. `MissionHandler.handle_episode_closed` receives `EpisodeClosureReport(outcome=SuccessContinue)` (`mission/handler.py:163`): `create_continuation_episode_with_manager(previous_episode=…)` inserts new Episode with `creation_reason=PARTIAL_CONTINUATION` and `goal=previous_episode.continuation_goal`, starts initial Attempt.

Invariant: `assert_continuation_episode_predecessor` (`mission/validation.py:38`) requires predecessor Episode is `SUCCEEDED` with non-null `continuation_goal`.

## Recursive (nested) missions

Generator (executor-profile) calls `request_mission_solution(goal=…)` (`tools/submission/main_agent/generator/request_mission_solution.py:59`).

`ExecutorSubmissionContext.start_mission_request` (`submission/context/executor.py:93`) → `MissionStarter.start(parent_task_id, goal)` (`mission/starter.py:53`):
1. Asserts parent task is RUNNING and has no open child Mission.
2. `MissionHandler.create_mission(requested_by_task_id=parent_task_id)`.
3. `create_initial_episode_with_manager` + `create_unstarted_initial_attempt`.
4. Parks parent task in `WAITING_COMPLEX_TASK` (entry-mode via `EntryTaskController.mark_waiting_mission`; attempt-mode via CAS, `starter.py:185-215`).
5. `episode_manager.start_attempt(initial_attempt)` → orchestrator starts → planner agent launched. Calling agent run ended (terminal tool).

Child Mission close:
- `MissionHandler.close_mission` → `MissionCloseReportRouter.deliver(report)` (`mission/close_report_delivery.py:44`).
- Router checks parent task's `task_center_attempt_id`:
  - `None` (entry-mode) → `EntryTaskController.apply_mission_close_report`
  - Non-null → `AttemptOrchestratorRegistry.get(attempt_id).apply_mission_close_report`
- Parent task: `WAITING_COMPLEX_TASK` → DONE/FAILED; `dispatch_ready_work()` re-evaluates DAG.

## Dependency context

`generator_dag.py` manages the per-Attempt DAG.

- `PlannedGeneratorTask.deps: tuple[str, ...]` (`task/models.py:36`) — local_id strings within plan.
- `ordered_generator_tasks` (`generator_dag.py:17`) — topological sort; rejects duplicate ids, unknown deps, cycles.
- `dependency_task_ids(attempt_id, local_deps)` (`generator_dag.py:52`) — local→full task id.
- `needs[]` column stores full task ids of upstream deps.
- `ready_pending_generator_ids` (`generator_dag.py:72`) — DONE-deps filter; called by dispatcher.
- `blocked_descendant_ids` (`generator_dag.py:90`) — BFS-marks downstream PENDING as BLOCKED on failure.

`ContextScope` (`context_engine/scope.py`) carries `mission_id`, `episode_id`, `attempt_id`, `task_id` into `ContextComposer.compose` (`agent_launch/composer.py:43`): resolver → engine.build → optional extra blocks → persist packet → renderer → `LaunchBundle`.

## Stores

All five are production SQLAlchemy implementations on a shared session factory. **There are no separate "in-memory" class variants** — in-memory setup uses SQLite `sqlite:///:memory:` with the same store classes.

| Store | Import | Purpose |
|---|---|---|
| `TaskCenterStore` | `db/stores/task_center_store.py` | Run rows, task rows |
| `MissionStore` | `db/stores/mission_store.py` | Mission CRUD |
| `EpisodeStore` | `db/stores/episode_store.py` | Episode CRUD |
| `AttemptStore` | `db/stores/attempt_store.py` | Attempt CRUD |
| `ContextPacketStore` | `db/stores/context_packet_store.py` | Persists rendered ContextPacket per launch |

**In-memory factory:** `create_in_memory_task_center_stores()` (`benchmarks/sweevo/live_test/stores.py:35`) — SQLite `:memory:` engine, `Base.metadata.create_all`, all five stores on shared `sessionmaker`, returns `TaskCenterStoreBundle`. **Same store implementations as production.**

Pytest fixture: `stores()` in `benchmarks/sweevo/live_test/fixtures.py:90` yields `TaskCenterStoreBundle` and disposes engine on teardown.

## AttemptAgentLauncher seam

**Seam #1 (ABOVE the query loop) — `runner=`:**

`EphemeralAttemptAgentLauncher` (`agent_launch/launcher.py:38`) accepts optional `runner: AttemptAgentRunner | None` (`launcher.py:50`).

`AttemptAgentRunner = Callable[..., Awaitable[Any]]` (`launcher.py:34`).

When `runner=None` → falls back to `engine.api.run_ephemeral_agent` (real LLM loop, `launcher.py:101-103`). When provided → called instead, **entirely replacing the model query loop**. Receives `(config, task_input, agent_def=…, sandbox_id=…, persist_agent_run=…, task_id=…, on_event=…, extra_tool_metadata=…)` (`launcher.py:117-126`).

`extra_tool_metadata` is `ExecutionMetadata` carrying `attempt_runtime`, `composer`, all id fields — runner can call real submission tools without touching the model.

`runner=` plumbed through `start_task_center_entry_run` → `TaskCenterEntryCoordinator.__init__` → `EphemeralAttemptAgentLauncher(runner=runner)` (`coordinator.py:110-115`, `coordinator.py:205`).

**Existing example:** `MockSquadRunner` (`benchmarks/sweevo/live_test/squad/runner.py:1`) is the SWE-EVO benchmark's `AttemptAgentRunner`.

**Seam #2 (BELOW seam #1, inside the query loop) — `QueryContext.api_client`:** see "Engine + Query Loop + LLM Seam" wiki page. **The two seams coexist.** Seam #1 = full runner replacement; seam #2 = LLM-only mock with the real query loop running.

After `start_task_center_entry_run` returns: `await handle.launcher.wait_for_idle()` (`launcher.py:76`) drains all recursively spawned asyncio tasks (child Missions, retried Attempts, continuation Episodes).

## What the live-e2e framework needs

### Public API surface

| Symbol | File |
|---|---|
| `start_task_center_entry_run` | `task_center/entry/coordinator.py:71` |
| `TaskCenterEntryHandle` | `task_center/entry/coordinator.py:57` |
| `AttemptAgentRunner` | `task_center/agent_launch/launcher.py:34` |
| `create_in_memory_task_center_stores` | `benchmarks/sweevo/live_test/stores.py:35` |
| `TaskCenterStoreBundle` | `benchmarks/sweevo/live_test/stores.py:23` |
| `TaskCenterSandboxBridge` | `task_center/entry/sandbox_bridge.py:34` (pass `start_fn=lambda sid: {"id": sid}` to bypass real Daytona) |
| `EpisodeCreationReason`, `AttemptFailReason`, `AttemptStatus`, `EpisodeClosureReport`, `SuccessContinue`, `TerminalSuccess`, `AttemptPlanFailed` | `task_center/api.py` |
| `HarnessTaskStatus`, `HarnessTaskRole` | `task_center/task/models.py` |

### Real vs fake

| Component | Status |
|---|---|
| `task_center` pipeline (coordinator/orchestrator/dispatcher/episode manager/mission handler/close-report router) | REAL |
| `TaskCenterStore`, `MissionStore`, `EpisodeStore`, `AttemptStore` | REAL (SQLite `:memory:`) |
| Terminal submission tools | REAL |
| `ContextComposer` + `ContextEngine` + recipes | REAL |
| Model API (`stream_message` / `run_ephemeral_agent`) | FAKE — replaced via runner= or api_client= |
| Sandbox (Daytona) | REAL in live tests |

### "What to Test" coverage

- **Real task_center pipeline workflow execution correctness** — full state machine runs with real stores.
- **Attempt retry on failure** — supply runner that submits failure or returns without terminal; assert `attempt_sequence_no=2` after retry.
- **Episodic continuation** — runner calls `submit_partial_plan(continuation_goal=…)`; assert second Episode with `creation_reason=PARTIAL_CONTINUATION`.
- **Initial mission** — first Episode `creation_reason=INITIAL`, `sequence_no=1`, Mission `status=succeeded`.
- **Nested mission** — runner calls `request_mission_solution`; assert child Mission with `requested_by_task_id=<gen_task_id>` and close report routed back.
- **Dependency context** — multi-task plan with `deps`; assert PENDING tasks not launched until `needs[]` are DONE; assert blocked descendants on failure.
- **Planner validation** — invalid plan (duplicate local_id, unknown dep, partial without `continuation_goal`); assert orchestrator raises `TaskCenterInvariantViolation` and attempt closes `fail_reason=planner_failed`.

---

## Update (2026-05-10T11:58:10.634Z)

## See also

- [[live-e2e-testing-framework-design]] — how the framework drives the pipeline
- [[engine-query-loop-llm-seam]] — what runs inside each agent task
- [[context-engine-recipes]] — how prompts are built per role
- [[sandbox-subsystem]] — what tool calls hit

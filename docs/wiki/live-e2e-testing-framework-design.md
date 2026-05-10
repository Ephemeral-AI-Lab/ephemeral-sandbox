---
title: "Live E2E Testing Framework — Design"
tags: ["live-e2e", "framework-design", "synthesis", "scenario-driven", "migration", "load-bearing"]
created: 2026-05-10T11:54:10.805Z
updated: 2026-05-10T20:30:00.000Z
sources: []
links: ["sandbox-subsystem.md", "task-center-pipeline.md", "context-engine-recipes.md", "engine-query-loop-llm-seam.md", "tools-hooks-guardrails-agents-notifications-messages.md"]
category: decision
confidence: medium
schemaVersion: 1
---

# Live E2E Testing Framework — Design

_Drafted 2026-05-10. Redrafted 2026-05-10 to reflect the migration mandate: the existing scenario harness under `backend/src/benchmarks/sweevo/live_test/` IS the framework. This document records what to lift out of that sub-tree into a project-wide top-level module, and the conventions (sandbox setup, task-center wiring, agent registration, scenario protocol, audit layout) the lifted module preserves verbatim._

## TL;DR — central design choice

**Lift the working harness out of `benchmarks/sweevo/live_test/` into a top-level `backend/src/live_e2e/` module. Standardize on scenario-driven `runner=` swap. Standardize all artifacts under `.sweevo_runs/scenario_logs/<scenario>/<UTCstamp>_<run_id>/`.**

This means:

1. The `runner=` seam (Seam #1 in the prior draft) is the project's e2e seam. `start_task_center_entry_run(..., runner=MockSquadRunner(...))` runs the real coordinator/orchestrator/dispatcher/episode-manager pipeline against real in-memory stores, with mock agents that call **real submission tools** through `execute_tool_once`. The query loop is bypassed by design — that is the point of a fast, deterministic, fidelity-where-it-matters e2e harness.
2. The `FakeReplayApiClient` (Seam #2) framing from the prior draft is **deferred** — out of scope for the migration. Future query-loop fidelity work can layer on top, but the migration's mandate is to generalize what already works.
3. Sandbox setup, task-center store bootstrap, agent definitions, scenario protocol, hook registry, audit recorder, and on-disk log layout are all **copied verbatim** (with `sweevo_*` renamed to neutral names) from `backend/src/benchmarks/sweevo/live_test/`.

The user's spec sentence remains the contract: "tool guardrail, hooks, max step, terminal hook submission, system notification trigger are taking effect." Those run inside `execute_tool_once` (not inside the query loop), so they fire under the `runner=` seam. The existing `MockSquadRunner._call_tool` already invokes `execute_tool_once`, which means real tool guardrails, real submission hooks, real terminal stamping, and real `BatchValidator` already run today.

## Migration: what moves and where it lands

### Source — current location

```
backend/src/benchmarks/sweevo/live_test/
  __init__.py
  runner.py                       # run_scenario(...) — orchestration entry point
  stores.py                       # TaskCenterStoreBundle + create_in_memory_task_center_stores
  fixtures.py                     # pytest fixtures: sweevo_sandbox, audit_dir, stores
  audit/
    bus.py                        # AuditEventBus
    events.py                     # EventType enum + Event dataclass
    node_id.py                    # NodeId
    recorder.py                   # AuditRecorder + 5 SQLAlchemy listeners
    metrics.py                    # MetricsAggregator
    lifecycle_observer.py
    sandbox_events.py             # sandbox_events_from_tool_completion
    stream_bridge.py              # StreamEvent → audit Event translator
    summary.py
  hooks/
    registry.py                   # Hook + HookSet + HookResult + MutableMockState
    builtins.py
  squad/
    runner.py                     # MockSquadRunner — role-based dispatch
    definitions.py                # registered_mock_sweevo_agents context manager
    tool_scripts.py               # PreparedToolScriptEngine + canned scripts
    full_stack_tool_scripts.py
    sandbox_probe.py
    prompt_inspector.py
  scenarios/
    base.py                       # Scenario protocol + ScenarioBase + ScenarioContext + ToolCallSpec
    correctness_testing.py
    full_case_user_input.py
    full_stack_adversarial.py
    user_input.py
  tests/
    conftest.py                   # pytest_plugins = ["benchmarks.sweevo.live_test.fixtures"]
    test_correctness.py
    test_full_case_user_input.py
    test_full_stack_adversarial.py
```

### Destination — generalized location

```
backend/src/live_e2e/
  __init__.py
  runner.py                       # run_scenario(...) — generalized
  stores.py                       # TaskCenterStoreBundle (unchanged)
  fixtures.py                     # generic_sandbox, audit_dir, stores
  audit/                          # copy as-is
  hooks/                          # copy as-is
  squad/
    runner.py                     # MockSquadRunner (de-sweevo-fied)
    definitions.py                # registered_mock_agents (renamed)
    tool_scripts.py               # generalized prepared-script engine
    sandbox_probe.py              # generic
    prompt_inspector.py           # unchanged
  scenarios/
    base.py                       # Scenario / ScenarioBase / ScenarioContext / ToolCallSpec
    [one file per scenario — see "Test scenarios" below]
  tests/
    conftest.py                   # pytest_plugins = ["live_e2e.fixtures"]
    test_<scenario>.py            # one per scenario
```

The SWE-EVO-specific bits (`benchmarks/sweevo/dataset.py`, `models.py`, `prompt.py`, `sandbox.py`, `evaluation.py`) **stay** under `benchmarks/sweevo/` — they are dataset-specific. The lifted `live_e2e/` module takes a `SandboxProvisioner` + `EntryPromptBuilder` callable so SWE-EVO scenarios can pass `create_sweevo_test_sandbox` and `build_sweevo_user_prompt` and any other consumer can pass their own.

### Hand-off API for SWE-EVO consumers

`benchmarks/sweevo/live_test/__init__.py` becomes a thin re-export shim that wires the SWE-EVO dataset into the generic framework:

```python
from live_e2e import run_scenario as _generic_run_scenario
from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
from benchmarks.sweevo.prompt import build_sweevo_user_prompt

async def run_sweevo_scenario(scenario, *, instance, **kwargs):
    return await _generic_run_scenario(
        scenario,
        sandbox_provisioner=lambda: create_sweevo_test_sandbox(instance),
        entry_prompt=build_sweevo_user_prompt(instance),
        **kwargs,
    )
```

Existing `benchmarks/sweevo/live_test/tests/test_*.py` files keep working through the shim; new tests target the generalized `live_e2e/` module.

## Sandbox setup steps (preserved verbatim from `benchmarks/sweevo/sandbox.py`)

The lifted framework documents the canonical sandbox-bring-up recipe. Any consumer (SWE-EVO, custom dataset, hand-built scenario) implements a `SandboxProvisioner` that follows these steps:

```python
# 1. Bootstrap Daytona provider (one-time per process)
from sandbox.provider.daytona.bootstrap import bootstrap_daytona_provider
bootstrap_daytona_provider()

# 2. Resolve / register the snapshot
#    - if image has explicit non-latest version → register Daytona snapshot
#    - else → use image directly via `image=...`
resolved_snapshot = resolve_snapshot(image=...)            # subprocess "daytona snapshot create"

# 3. Reuse-existing-auto OR fresh-create
#    - reuse: filter list_sandboxes by labels {purpose, project_dir, dataset_instance}
#             prefer started > stopped, exclude pending_build/error/build_failed
#    - fresh: prune stale auto-sandboxes, then sandbox_api.create_sandbox(name=, language="python", labels={...}, snapshot=|image=)
sandbox = sandbox_api.create_sandbox(name=..., snapshot=resolved_snapshot, language="python", labels={...})
sandbox_id = sandbox["id"]

# 4. Wait for exec readiness — bounded retry on "connection reset" / "server disconnected"
await wait_for_sandbox_exec_ready(sandbox_id, attempts=6, delay_s=1.0)

# 5. Repo checkout — git reset --hard, git clean -fd, git checkout -f <base_commit>, git checkout -B work-branch
await exec(sandbox_id, f"cd {repo_dir} && git reset --hard HEAD")
await exec(sandbox_id, f"cd {repo_dir} && git clean -fd")
await exec(sandbox_id, f"cd {repo_dir} && git checkout -f {base_commit}")
await exec(sandbox_id, f"cd {repo_dir} && git checkout -B work-branch {base_commit}")

# 6. Editable install (best-effort)
await exec(sandbox_id, f"{conda_activate} && cd {repo_dir} && pip install -e . -q || true", timeout=600)

# 7. Rebuild public-tool workspace base — REQUIRED for sandbox_toolkit tools to work
from sandbox.host.daemon_client import call_daemon_api
await call_daemon_api(sandbox_id, "api.build_workspace_base", {"workspace_root": repo_dir, "reset": True}, timeout=240)

# 8. Confirm runtime ready
readiness = await call_daemon_api(sandbox_id, "api.runtime.ready", {}, timeout=60)
assert readiness["success"] and readiness["ready"]

# 9. (Optional) apply test patch via base64 chunked upload — git apply --check first to detect already-applied
await ensure_test_patch(sandbox_id, repo_dir, test_patch)

# 10. Per-test workspace reset — rerun steps 5–7 between tests in the same session
```

Critical constraint: **step 7 is non-optional**. Skipping `api.build_workspace_base` leaves the workspace daemon in an inconsistent state and `read_file`/`edit_file`/`write_file` tools will report `workspace_not_ready` errors.

The `workspace` pytest fixture pattern from `benchmarks/sweevo/live_test/fixtures.py` (cache-keyed first-call skip, subsequent reset via `reset_workspace`) is the pattern to keep.

## Task-center setup (real PostgreSQL via `db.engine.initialize_db`)

The framework uses the **existing project PostgreSQL** instead of per-test in-memory SQLite. This trades raw test speed for production-fidelity (real Postgres types, real concurrency semantics, real migration code path) and removes the dialect drift that made `_rebuild_sqlite_table()` necessary in `db.engine`.

### Connection wiring

The shared engine is created once per process by `db.engine.initialize_db()` (`backend/src/db/engine.py:276`), which reads `EPHEMERALOS_DATABASE_URL` (or `DatabaseSettings.url`) and produces both a sync `sessionmaker` and an async `async_sessionmaker`. It also runs `Base.metadata.create_all`, `_rename_columns`, `_add_missing_columns`, and `_drop_legacy_tables` — the canonical migration path.

The framework reuses this engine; it does not call `create_engine()` itself. Per-test isolation comes from a fresh **PostgreSQL schema** carved out of the same database, so tests never collide and never touch the production `public` schema.

### Per-test schema isolation

```python
# live_e2e/stores.py
from contextlib import contextmanager
from dataclasses import dataclass
from uuid import uuid4

from sqlalchemy import Engine, MetaData, event, text
from sqlalchemy.orm import Session, sessionmaker

from db.base import Base
import db.models  # noqa: F401 — populate SQLAlchemy metadata
from db.engine import get_engine, get_session_factory, initialize_db
from db.stores.attempt_store import AttemptStore
from db.stores.context_packet_store import ContextPacketStore
from db.stores.episode_store import EpisodeStore
from db.stores.mission_store import MissionStore
from db.stores.task_center_store import TaskCenterStore


@dataclass(slots=True)
class TaskCenterStoreBundle:
    engine: Engine                           # shared project engine (DO NOT dispose)
    schema: str                              # per-test schema name
    session_factory: sessionmaker[Session]   # bound to engine, search_path = <schema>
    task_store: TaskCenterStore
    mission_store: MissionStore
    episode_store: EpisodeStore
    attempt_store: AttemptStore
    context_packet_store: ContextPacketStore

    def close(self) -> None:
        """Drop the per-test schema. Engine itself is shared — never dispose it."""
        with self.engine.begin() as conn:
            conn.execute(text(f'DROP SCHEMA IF EXISTS "{self.schema}" CASCADE'))


def _ensure_initialized() -> Engine:
    engine = get_engine()
    if engine is None:
        initialize_db()                       # reads EPHEMERALOS_DATABASE_URL
        engine = get_engine()
    if engine is None:
        raise RuntimeError(
            "EPHEMERALOS_DATABASE_URL not configured — set it to the project PostgreSQL "
            "DSN before running live_e2e tests."
        )
    if engine.dialect.name != "postgresql":
        raise RuntimeError(
            f"live_e2e requires PostgreSQL, got dialect={engine.dialect.name!r}"
        )
    return engine


def _bind_search_path(session_factory: sessionmaker[Session], schema: str) -> None:
    """Force every checked-out connection to SET search_path = <schema>, public."""

    @event.listens_for(session_factory.kw["bind"], "connect")
    def _set_search_path(dbapi_conn, connection_record):  # noqa: ARG001
        with dbapi_conn.cursor() as cur:
            cur.execute(f'SET search_path TO "{schema}", public')


def create_per_test_task_center_stores(
    *, schema_prefix: str = "live_e2e"
) -> TaskCenterStoreBundle:
    """Carve a fresh schema, run create_all against it, return wired stores."""
    engine = _ensure_initialized()
    schema = f"{schema_prefix}_{uuid4().hex[:12]}"

    with engine.begin() as conn:
        conn.execute(text(f'CREATE SCHEMA "{schema}"'))

    # Re-target metadata to the per-test schema by cloning each Table.
    test_metadata = MetaData(schema=schema)
    for table in Base.metadata.sorted_tables:
        table.tometadata(test_metadata, schema=schema)
    test_metadata.create_all(engine)

    session_factory = sessionmaker(
        bind=engine, autoflush=False, expire_on_commit=False
    )
    _bind_search_path(session_factory, schema)

    bundle = TaskCenterStoreBundle(
        engine=engine,
        schema=schema,
        session_factory=session_factory,
        task_store=TaskCenterStore(),
        mission_store=MissionStore(),
        episode_store=EpisodeStore(),
        attempt_store=AttemptStore(),
        context_packet_store=ContextPacketStore(),
    )
    for store in (bundle.task_store, bundle.mission_store, bundle.episode_store,
                  bundle.attempt_store, bundle.context_packet_store):
        store.initialize(session_factory)
    return bundle
```

Why per-schema (not per-database, not transaction-rollback, not truncate):
- **Per-database** (`CREATE DATABASE test_xyz`) requires a separate `create_engine` per test — defeats connection pooling and adds 1–3s of overhead per test.
- **Transaction-rollback** doesn't work cleanly because the orchestrator/dispatcher commits internally on every state transition; a wrapping `BEGIN ... ROLLBACK` would either deadlock or no-op.
- **Truncate-after-test** on a shared schema serializes all tests against one `public` namespace, leaks state on crash, and races the AuditRecorder's `after_insert` listeners on cleanup.
- **Per-schema** gives full isolation, pools the same engine, drops cleanly via `DROP SCHEMA ... CASCADE`, and matches Postgres's intended multi-tenant pattern.

### What the AuditRecorder listeners attach to

`AuditRecorder.start()` registers `event.listen(MissionRecord, "after_insert", ...)` and similar on the four other ORM record classes. These listeners are **engine-agnostic** — they fire on any SQLAlchemy session that flushes those mappers, regardless of which schema the underlying tables live in. So the recorder works unchanged against the per-test schema.

### Pytest fixture (skip when Postgres not configured)

```python
# live_e2e/fixtures.py
@pytest.fixture
def stores() -> Iterator[TaskCenterStoreBundle]:
    if not os.environ.get("EPHEMERALOS_DATABASE_URL"):
        pytest.skip("EPHEMERALOS_DATABASE_URL not set — live_e2e requires PostgreSQL")
    bundle = create_per_test_task_center_stores()
    try:
        yield bundle
    finally:
        bundle.close()                       # DROP SCHEMA ... CASCADE
```

The `stores` fixture is function-scoped (one schema per test). The shared engine is process-scoped via `initialize_db()`'s module-level `_engine`.

### Concurrent-test safety

`pytest -n auto` (xdist) is safe: each worker process calls `initialize_db()` once, each test creates its own schema. Schema names use `uuid4().hex[:12]` so collision probability is negligible. The `public` schema (which `initialize_db` populates with the production tables) is never written to by tests because the per-connection `search_path` puts the test schema first.

### Migration cost

The first call to `initialize_db()` per process runs `Base.metadata.create_all(public)` plus the rename/add/drop migration steps. This is one-time per process. Per-test schema creation only runs `test_metadata.create_all(<schema>)` (the cloned-metadata `Base`-equivalent against the new schema), which is fast (~50–150ms for the full table set on local Postgres).

## Agent setup (preserved from `squad/definitions.py`)

The framework registers a fixed five-role squad before each scenario via a context manager that swaps the global agent registry, then restores it:

```python
# live_e2e/squad/definitions.py
@contextlib.contextmanager
def registered_mock_agents() -> Iterator[None]:
    """Temporarily install the minimal TaskCenter squad definitions."""
    previous = list_definitions()
    for d in previous:
        unregister_definition(d.name)
    for d in mock_agent_definitions():
        register_definition(d)
    try:
        yield
    finally:
        for d in list_definitions():
            unregister_definition(d.name)
        for d in previous:
            register_definition(d)

def mock_agent_definitions() -> tuple[AgentDefinition, ...]:
    return (
        AgentDefinition(name="entry_executor", role="executor",
            context_recipe="entry_executor_v1",
            terminals=["request_mission_solution", "submit_execution_success", "submit_execution_failure"]),
        AgentDefinition(name="planner", role="planner",
            context_recipe="planner_v1",
            terminals=["submit_full_plan", "submit_partial_plan"]),
        AgentDefinition(name="executor", role="executor",
            context_recipe="generator_v1",
            allowed_tools=["read_file", "write_file", "edit_file", "shell"],
            terminals=["request_mission_solution", "submit_execution_success", "submit_execution_failure"]),
        AgentDefinition(name="verifier", role="verifier",
            context_recipe="generator_v1",
            allowed_tools=["read_file", "shell"],
            terminals=["submit_verification_success", "submit_verification_failure"]),
        AgentDefinition(name="evaluator", role="evaluator",
            context_recipe="evaluator_v1",
            terminals=["submit_evaluation_success", "submit_evaluation_failure"]),
    )
```

Why this matters: the `MockSquadRunner` dispatches on `agent_def.role` (planner/executor/verifier/evaluator) and on `agent_def.name == "entry_executor"`. Scenarios decide what each role submits via the `Scenario` protocol's four decision methods. Real `terminals` and `allowed_tools` fields drive real terminal stamping and real allowed-tool guardrails, so guardrail/exclusivity behavior is exercised.

## Scenario protocol (preserved from `scenarios/base.py`)

A scenario is a class with four pure decision methods:

```python
# live_e2e/scenarios/base.py
@dataclass(frozen=True, slots=True)
class ToolCallSpec:
    tool: Any  # BaseTool
    args: dict[str, Any]

@dataclass(slots=True)
class ScenarioContext:
    attempt: Any        # Attempt | None
    episode: Any        # Episode | None
    mission: Any        # Mission | None
    prompt: str
    metadata: Any       # ExecutionMetadata
    audit_recorder: Any
    mutable_state: Any
    task_id: str | None = None
    agent_name: str | None = None
    task_input: str | None = None
    graph_summary: dict[str, Any] | None = None

class ScenarioBase:
    name: str = ""
    expected_event_sequence: tuple[EventType, ...] = ()

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec: ...
    def executor_actions(self, ctx: ScenarioContext) -> Sequence[Any]: ...
    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec: ...
    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec: ...
    def recursive_mission_goal(self, ctx: ScenarioContext) -> str | None: ...
    def hooks(self) -> Sequence[Hook]: ...
```

The `MockSquadRunner` calls these at the appropriate stage. The scenario sees live `Attempt`, `Episode`, and `Mission` records — so it can branch on `episode.sequence_no`, `attempt.attempt_sequence_no`, `attempt.fail_reason`, etc., to drive multi-attempt and multi-episode flows. `executor_actions` returns a sequence of action strings (`"preflight"`, `"sandbox_integrity"`, `"final_probe"`, `"execute_package:<pkg>"`, `"request_recursive_mission:<pkg>"`, etc.) the runner translates into real tool sequences.

## Audit recording + on-disk log layout

The `AuditRecorder` registers five SQLAlchemy listeners (`MissionRecord`, `EpisodeRecord`, `AttemptRecord`, `TaskCenterTaskRecord`, `AgentRunRecord`) plus subscribes to the in-memory `AuditEventBus`. Every commit fires a handler that writes a `*.json` snapshot to a hierarchical directory; per-task message streams append to `message.jsonl`; sandbox-derived events append to `sandbox_events.jsonl`.

### Canonical run-directory layout

`run_dir = <audit_dir>/scenario_logs/<scenario_name>/<UTCstamp>_<short_run_id>/`

Where `audit_dir` defaults to `<repo>/.sweevo_runs/` (kept as the canonical name during migration to avoid disrupting the existing artifact tree consumers). Override via `EOS_SWEEVO_AUDIT_DIR` or `EOS_SWEEVO_AUDIT_TMP=1`.

```
.sweevo_runs/scenario_logs/<scenario_name>/<UTCstamp>_<short_run_id>/
  run.json                                     # task_center_run_id, scenario_name, instance_id, sandbox_id, started_ts, finished_ts, status
  metrics.json                                 # MetricsAggregator snapshot
  sandbox_events.jsonl                         # sandbox-derived events from tool completions
  entry_executor_<task_id>:entry/
    task.json                                  # TaskCenterTaskRecord snapshot
    message.jsonl                              # AgentMessageJsonlRecorder stream (one message per line)
  mission_<NN>_<mission_id>/
    mission.json                               # MissionRecord snapshot
    episode_<NN>_<episode_id>/
      episode.json                             # EpisodeRecord snapshot
      attempt_<NN>_<attempt_id>/
        attempt.json                           # AttemptRecord snapshot
        01_planner_<task_id>:planner/
          task.json
          message.jsonl
        02_executor_<task_id>:gen:<local_id>/  # role inferred via _display_role: generator/executor/verifier
          task.json
          message.jsonl
        03_evaluator_<task_id>:evaluator/
          task.json
          message.jsonl
```

### Concrete example (scenario: correctness_testing)

```
.sweevo_runs/scenario_logs/correctness_testing/20260509T101551Z_6da9df1615e3/
  run.json                                                          # 290 B
  metrics.json                                                      # 1.7 KB
  sandbox_events.jsonl                                              # 29 KB
  entry_executor_5da2f268-...:entry/
    task.json                                                       # 193 KB
    message.jsonl                                                   # 291 KB
  mission_01_5bf4504d-.../
    mission.json                                                    # 96 KB
    episode_01_bc1458f7-.../
      episode.json
      attempt_01_3b913432-.../                                      # planner full plan, evaluator failure
        attempt.json
        01_planner_3b913432-...:planner/{task.json, message.jsonl}
        02_executor_3b913432-...:gen:preflight/{task.json, message.jsonl}
        03_evaluator_3b913432-...:evaluator/{task.json, message.jsonl}
      attempt_02_b3bdaf7a-.../                                      # partial plan retry, evaluator success
        attempt.json
        01_planner_b3bdaf7a-...:planner/...
        02_executor_b3bdaf7a-...:gen:sandbox_integrity/...          # exercises read/write/edit/shell + batch + conflict
        03_evaluator_b3bdaf7a-...:evaluator/...
    episode_02_997d6b55-.../                                        # PARTIAL_CONTINUATION
      episode.json
      attempt_01_fcfaaaf9-.../
        ...
```

`run.json` example payload (verbatim shape):
```json
{"task_center_run_id": "5da2f268-...", "scenario_name": "correctness_testing",
 "instance_id": "dask__dask_2023.3.2_2023.4.0", "sandbox_id": "2e654ee1-...",
 "started_ts": 1778321751.26, "finished_ts": 1778321759.49, "status": "finished"}
```

### Layout invariants

- `mission_<NN>_<id>` numbering is per-recorder-instance monotonic (1-based, 2-digit zero-pad).
- `episode_<NN>_<id>` numbering resets per mission. Same for `attempt_<NN>_<id>` per episode.
- Per-attempt children (`01_planner_*`, `02_executor_*`, `03_evaluator_*`) are numbered in commit order via `_role_seq_counter[attempt_id]`.
- Suffixes after `:` come from `task.spawn_reason` / `task.local_id` and disambiguate generator subtasks.
- `_display_role` rule: a `role="generator"` task with `agent_name in {"executor","verifier"}` is shown as `executor` / `verifier` in the directory name (so a generator-stage task named `executor` lands in `02_executor_<id>:gen:<local_id>/`).
- `entry_executor_*` tasks live at the run-dir top level (not under any mission), keyed by `is_entry_executor()` predicate.
- `task.json` is rewritten atomically (tmp + `os.replace`) on every `after_update`. Latest state always present.
- `message.jsonl` is append-only (real `AgentMessageJsonlRecorder`).

## Wiring (the runtime assembly)

```python
# 1. One-time setup (provider bootstrap, recipe registration, predicate registration, tool registration, agent tree load)
bootstrap_daytona_provider()
register_builtin_recipes()
register_builtin_predicates()
register_built_in_tools_against(tool_registry)

# 2. Per-test sandbox (REAL Daytona) — provided by the consumer's SandboxProvisioner
sandbox_info = await create_test_sandbox(...)
sandbox_id = sandbox_info["sandbox_id"]

# 3. Per-test stores (in-memory SQLite, real schema)
bundle = create_in_memory_task_center_stores()

# 4. Per-test audit bus + recorder. Recorder is constructed BEFORE start_task_center_entry_run
#    so initial Mission/Episode/Task commits are captured. task_center_run_id is bound post-fact.
self_run_id = uuid.uuid4().hex[:12]
run_dir = audit_dir / "scenario_logs" / scenario.name / f"{utcstamp()}_{self_run_id}"
bus = AuditEventBus()
recorder = AuditRecorder(run_dir, task_center_run_id="", bus=bus,
                         scenario_name=scenario.name, sandbox_id=sandbox_id)
recorder.start()

# 5. Hooks + mutable state (cross-firing scratchpad)
mutable_state = MutableMockState()
hook_set = HookSet()
for hook in scenario.hooks():
    hook_set.register(hook)

def _on_event(event: Event) -> None:
    captured_events.append(event)
    mutable_state.seen_events.append(event.type)
    for result in hook_set.fire(event, "post", mutable_state):
        hook_results.append(result)
bus.subscribe(_on_event)

# 6. Squad runner — calls real submission tools through execute_tool_once
with registered_mock_agents():
    squad = MockSquadRunner(
        bus=bus,
        task_center_run_id="",                  # late-bound below
        scenario=scenario,
        mutable_state=mutable_state,
        audit_recorder=recorder,
    )
    handle = start_task_center_entry_run(
        config=SimpleNamespace(cwd=repo_dir),
        prompt=entry_prompt_text,
        sandbox_id=sandbox_id,
        on_agent_event=stream_bridge(bus, task_center_run_id="<late>"),  # also writes per-task message.jsonl
        task_store=bundle.task_store,
        mission_store=bundle.mission_store,
        episode_store=bundle.episode_store,
        attempt_store=bundle.attempt_store,
        context_packet_store=bundle.context_packet_store,
        runner=squad,                           # ← THE seam — replaces engine.api.run_ephemeral_agent
        sandbox_bridge=TaskCenterSandboxBridge(start_fn=lambda existing_id: {"id": existing_id}),
    )
    tcrid = str(handle.task_center_run_id)
    squad._task_center_run_id = tcrid           # late binding
    recorder.bind_task_center_run_id(tcrid)
    bus.publish(Event(type=EventType.RUN_STARTED, node=NodeId(task_center_run_id=tcrid)))
    await handle.launcher.wait_for_idle()
    bus.publish(Event(type=EventType.RUN_COMPLETED, node=NodeId(task_center_run_id=tcrid)))

# 7. Build report; assert
report = RunReport(scenario_name=scenario.name, task_center_run_id=tcrid,
                   sandbox_id=sandbox_id, run_dir=run_dir,
                   task_center_status=bundle.task_store.get_run(tcrid)["status"],
                   events=captured_events, hook_results=hook_results,
                   metrics=recorder.metrics.snapshot(),
                   graph_summary=_graph_summary(bundle, tcrid))

# 8. Cleanup
recorder.dispose()
bundle.close()
```

Critical ordering details (preserved from `runner.py`):
- Recorder constructed and `recorder.start()` called BEFORE `start_task_center_entry_run` — otherwise the synchronous initial Mission/Episode/Task commits race past the listener registration.
- `task_center_run_id` is bound post-fact via `recorder.bind_task_center_run_id(tcrid)` — the entry coordinator generates the id internally.
- `squad._task_center_run_id = tcrid` is late-binding; the squad publishes events tagged with the eventual run id once known.
- `_on_agent_event` does double duty: `stream_bridge` translates `ToolExecutionStarted/Completed` into bus events; the same callback also routes per-agent-run StreamEvents into the task's `AgentMessageJsonlRecorder` via `recorder.message_recorder_for_agent_run(agent_run_id)`.

## Test scenarios — what to migrate, what to add

### Already implemented (lift verbatim)

| Scenario class | Module | What it covers |
|---|---|---|
| `CorrectnessTesting` | `scenarios/correctness_testing.py` | Entry → mission → episode 1 (attempt 1 fails; attempt 2 passes via partial plan) → continuation episode → mission close. Sandbox read/write/edit/shell + batch edit + conflict detection. |
| `FullCaseUserInput` | `scenarios/full_case_user_input.py` | Single composite scenario for the user-input ingest path, with `inspect_full_user_input` script. |
| `FullStackAdversarial` | `scenarios/full_stack_adversarial.py` | OCC conflict matrix, overlay edge matrix, layerstack squash lease, LSP refresh semantics, recursive oversized matrix, full-stack final reconciliation. |
| `UserInput` | `scenarios/user_input.py` | Lightweight user-input ingest scenario. |

### Test runner (one pytest test per scenario)

```python
# live_e2e/tests/test_correctness_testing.py
import pytest
from live_e2e import run_scenario
from live_e2e.scenarios.correctness_testing import CorrectnessTesting

@pytest.mark.asyncio
@pytest.mark.live_e2e            # marker so unit-test collections can deselect
async def test_correctness_testing(sandbox, audit_dir, stores, instance):
    scenario = CorrectnessTesting()
    report = await run_scenario(
        scenario,
        instance=instance,                    # provided by dataset-specific fixture
        sandbox_id=sandbox["sandbox_id"],
        audit_dir=audit_dir,
        stores=stores,
    )
    assert report.task_center_status == "succeeded"
    assert report.passed_prompt_inspections
    assert report.passed_sandbox_checks
    # tighter assertions
    assert tuple(report.seen_event_types) == scenario.expected_event_sequence
    # graph shape
    assert len(report.graph_summary["missions"]) == 1
    assert len(report.graph_summary["missions"][0]["episodes"]) == 2
```

Each scenario gets its own test file. Scenario state is pure (the four decision methods are deterministic given `ScenarioContext`), so the same scenario can be reused across tests with different fixtures.

### Future scenarios to add (post-migration)

| Concern | Scenario | Notes |
|---|---|---|
| Tool guardrail (request_mission_after_edit) | `scenario: gate_request_mission_after_edit` | executor_actions yields one `edit_file` then `request_mission_solution`; assert hook fires `HookResult.fail`. |
| Tool guardrail (resolver success limit) | `scenario: gate_resolver_success_limit` | exercise via subagent helper path. |
| Terminal-tool exclusivity | `scenario: terminal_exclusivity` | hand-craft a tool batch that mixes a terminal with a sibling read; assert `validate_tool_batch` rejects. |
| Max-step / `tool_call_limit` | `scenario: max_step_limit` | scenario sets `agent_def.tool_call_limit=3`; executor_actions yields 4 non-terminals. |
| Planner validation | `scenario: planner_validation_duplicate_local_id` | planner_response returns plan with duplicate `local_id`; assert attempt closes `fail_reason=planner_failed`. |
| Recursive mission | `scenario: nested_mission` | executor returns `request_recursive_mission:<pkg>`; assert child Mission with `requested_by_task_id`. |
| Dependency context | `scenario: dependency_results_block` | plan with `tasks=[{id="b",deps=["a"]}]`; assert `b`'s prompt contains `# Dependency Results` with `a`'s summary. |
| Episodic continuation | `scenario: episodic_continuation` | already covered by `CorrectnessTesting` (episode 2 via PARTIAL_CONTINUATION). |
| Attempt retry on each role | `scenario: retry_on_planner_fail` / `retry_on_generator_fail` / `retry_on_evaluator_fail` | partially covered by `CorrectnessTesting` (evaluator path). |
| Context correctness per role | `scenario: prompt_inspection_per_role` | exercises `MockSquadRunner._inspect_prompt` checks across all five roles. |

Each new scenario subclasses `ScenarioBase`, overrides the four decision methods, and ships in `scenarios/<name>.py`. The runner code does not change.

## Hooks system (preserved from `hooks/registry.py`)

Hooks are insertion-ordered, fire on `EventType` + `("pre"|"post")` from the bus, and can mutate `MutableMockState`. The state lets a hook inject failures (`inject_failure(role, attempt_id, checkpoint)`) consumed later by the squad runner, or replace the next planner response (`replace_next_planner_response(spec)`). Scenarios register their hooks via `Scenario.hooks()`. The runner concatenates `scenario.hooks()` and any `extra_hooks` argument.

## Open questions / follow-ups

1. **Module name.** `live_e2e` vs `e2e_harness` vs `scenario_harness`. Recommend `live_e2e` for symmetry with the existing `live_test/` sub-tree name.
2. **Audit dir env var rename.** `EOS_SWEEVO_AUDIT_DIR` and `EOS_SWEEVO_AUDIT_TMP` are dataset-coupled names. Either keep them for compatibility (they work fine) or alias to `EOS_E2E_AUDIT_DIR`/`EOS_E2E_AUDIT_TMP` and deprecate the old names. Recommend: dual-read both with `EOS_E2E_*` taking precedence; warn on `EOS_SWEEVO_*` after migration is done.
3. **Sandbox provisioner contract.** Settle on `Callable[[], Awaitable[dict[str, str|object]]]` returning `{"sandbox_id": str, "repo_dir": str, ...}` so scenarios that don't ship with a SWE-EVO instance can pass any provisioner.
4. **Per-test workspace reset.** The `workspace` fixture's "first call after fresh sandbox skips reset" pattern depends on `request.session.config.cache`. When migrated, decide whether to keep the session-cache trick or always reset.
5. **`EOS_TIER_RUN_ID` artifact stability.** Memory note `eos_tier_run_id_artifact_stability` indicates `live_e2e_test` artifacts honor `EOS_TIER_RUN_ID` for resume-on-restart. Make sure the lifted framework keeps writing under `${EOS_TIER_RUN_ID:-<self_run_id>}` so `run_tiered.py` resume contracts still hold.
6. **Future Seam #2 (FakeReplayApiClient).** Out of scope for this migration. If/when added, it would slot in alongside `runner=` as an alternative `external_api_client=` configuration on `RuntimeConfig`. The scenario protocol would extend with an `assistant_response(turn)` method for replay.

## Cross-references
- [[sandbox-subsystem]] — what the framework drives
- [[task-center-pipeline]] — what the framework asserts on
- [[context-engine-recipes]] — recipes the planner/executor/verifier/evaluator use
- [[engine-query-loop-llm-seam]] — the seam Seam #2 would swap (deferred)
- [[tools-hooks-guardrails-agents-notifications-messages]] — what runs real inside `execute_tool_once`

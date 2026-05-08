# SWE-EVO live e2e test framework — design plan

**Date:** 2026-05-08
**Branch:** codex/fix-dot-path-normalization-tests
**Scope:** Convert `backend/src/benchmarks/sweevo/` from a single-instance benchmark runner into an extensible, event-driven, persistence-rich live e2e test framework that exercises the real Daytona sandbox + real TaskCenter runtime with deterministic mock agents.

---

## 1. Goals

1. **Verify sandbox correctness at OS level** — read/write/edit consistency, layer-stack squash integrity, OCC direct merge vs OCC gated merge, batch edits, conflict detection.
2. **Verify TaskCenter graph correctness** — entry → mission → episode → attempt lifecycle, including episode retries (failed-attempt context), continuation episodes (partial plan + continuation goal), and nested sub-missions (`request_mission_solution` from inside an executor).
3. **Verify prompt context correctness** — every recipe (`entry_executor_v1`, `planner_v1`, `generator_v1`, `evaluator_v1`) emits the expected blocks given the current store state.
4. **Reproduce real-agent execution shape using deterministic mocks** — no API tokens spent, but the same TaskCenter runtime, the same context engine, the same sandbox tools, the same submission terminals.
5. **Frameworked extensibility** — adding a new scenario should be ~50 LOC. Hooks, event bus, audit recorder, fixtures are shared infrastructure.
6. **Auditability** — every run leaves a hierarchical, human-browseable directory tree on disk capturing context, conversations, state transitions, tool-call metrics.

---

## 2. Non-goals

- Real-agent grading bar (F2P/P2P) — `evaluation.py` stays for the optional real-agent CLI path, but the mock framework does not exercise it.
- Replacing the existing `live_e2e_test/` sandbox tier suite — the new framework lives under `src/benchmarks/sweevo/live_test/` and is wired in via a new tier entry.
- Sandbox-internal correctness beyond what `sandbox.api` exposes — we use the public surface only.

---

## 3. Decisions locked in this design pass

| # | Decision |
|---|---|
| 1 | **Default SWE-EVO instance:** `dask__dask_2023.3.2_2023.4.0` |
| 2 | **Sandbox isolation:** session-scoped Daytona sandbox; per-test `git reset --hard HEAD && git clean -fd && rm -rf .ephemeralos/sweevo-mock` only when reusing (skip on first test after fresh provision) |
| 3 | **Real-agent grading:** out of scope for this milestone |
| 4 | **Test suite location:** `backend/src/benchmarks/sweevo/live_test/` (the framework + tests live alongside sweevo source) |
| 5 | **Test taxonomy:** mocked full-execution scenarios + event-driven targeted functional scenarios (squash trigger after N edits, mid-run failure → retry, continuation episodes, pre/post hook injection points) |
| 6 | **Framework-style extensibility:** explicit `Scenario`/`Hook` protocols, event bus, scenario registry; orchestrated agent tool calls drive events |
| 7 | **Auditability:** hierarchical on-disk directory tree per run, grouped under `scenario_logs/<scenario_name>/<task_center_run_id>/` |
| 8 | **Per-agent persistence:** `task.jsonl` (full Task row snapshot per update) + `message.jsonl` (appended **mid-run on every message boundary**). **Helper/subagent invocations are filtered out** — only the primary roles (`entry_executor`, `planner`, `executor`, `evaluator`) get a directory. |
| 9 | **Per-lifecycle persistence:** append-only `mission.jsonl` / `episode.jsonl` / `attempt.jsonl` / `task.jsonl`. **One line per ORM update**, each line = one full record snapshot. Last line = current state; all lines = mutation history. Single `os.write(O_APPEND)` per write, no tmp+rename ceremony. |
| 10 | **DTOs and ORMs are self-contained:** `MissionRecord`, `EpisodeRecord`, `AttemptRecord` carry `context: TEXT` + `summary: TEXT`. `TaskCenterTaskRecord` carries `system_prompt: TEXT` + `user_prompt: TEXT` (the rendered ContextPacket is denormalized inline). The corresponding frozen dataclass DTOs (`Mission`, `Episode`, `Attempt`) gained `context: str \| None` + `summary: str \| None`. The on-disk JSONL line is just `dataclasses.asdict(dto)` (or `_serialize_task(record)`). No separate `context.txt` / `summary.txt`. |
| 11 | **No global event log:** `events.jsonl` is dropped. The in-memory event bus still drives the LifecycleObserver, hooks, and metrics aggregation — but events are not persisted. Chronological replay is reconstructed from DTO `updated_at` + per-agent `message.jsonl` timestamps. |
| 12 | **Audit dir location:** `<repo>/.sweevo_runs/` (gitignored); override via `EOS_SWEEVO_AUDIT_DIR` or `EOS_SWEEVO_AUDIT_TMP=1` |
| 13 | **Hook ordering:** insertion order |
| 14 | **Concurrency:** serial pytest first; xdist as a follow-up |
| 15 | **Scenario discovery:** explicit registry in `scenarios/__init__.py` |

---

## 4. Source-package narrowing

`src/benchmarks/sweevo/` keeps only the responsibilities the user explicitly authorized: dataset, sandbox provisioning, entry-prompt construction, real-agent grading, real-agent CLI.

### Stays
| File | Purpose |
|---|---|
| `__init__.py` | `NO_PROXY` shim for HTTPS proxy bypass |
| `dataset.py` | SWE-EVO instance loader, size classification, snapshot name |
| `models.py` | `SWEEvoInstance`, `SWEEvoResult`, constants |
| `sandbox.py` | Daytona snapshot register + sandbox provision + setup at `/testbed` |
| `evaluation.py` | F2P/P2P grading (real-agent only) |

### New
| File | Purpose |
|---|---|
| `prompt.py` | Extracted from `task_center_runner.py`: `build_sweevo_user_prompt`, `pr_description_for_instance`, `load_pr_description_overrides`, PR-description CSV constants |
| `__main__.py` (rewritten slim) | CLI: `--real-agent` (default agent loop) and `--scenario <name>` (delegates to live_test runner). No more bundled mock execution. |

### Deleted (moved into `live_test/`)
| File | Migration |
|---|---|
| `mock_agent_execution.py` | Split across `live_test/squad/{runner,prompt_inspector,sandbox_probe}.py`, `live_test/stores.py` |
| `task_center_runner.py` | `build_sweevo_user_prompt` → `prompt.py`; `run_sweevo_with_task_center` → `live_test/runner.py`; `_evaluate_mock_agent_execution_run` → `live_test/runner.py`; `_append_message_log` superseded by audit recorder |

---

## 5. Final file layout

```
backend/src/benchmarks/sweevo/
├── __init__.py                       # NO_PROXY shim
├── dataset.py                        # unchanged
├── models.py                         # unchanged
├── sandbox.py                        # unchanged
├── evaluation.py                     # unchanged
├── prompt.py                         # NEW — extracted from task_center_runner
├── __main__.py                       # slim — real-agent CLI + scenario shortcut
└── live_test/
    ├── __init__.py
    │
    ├── audit/
    │   ├── __init__.py
    │   ├── events.py                 # EventType enum + Event dataclass
    │   ├── node_id.py                # Hierarchical NodeId breadcrumb
    │   ├── bus.py                    # AuditEventBus (sync fanout to recorders + hooks)
    │   ├── recorder.py               # AuditRecorder — directory writer
    │   ├── stream_bridge.py          # StreamEvent → audit Event translation
    │   ├── lifecycle_observer.py     # diffs stores before/after squad call → emits MISSION_/EPISODE_/ATTEMPT_ events
    │   ├── metrics.py                # post-run aggregator → metrics.json
    │   └── summary.py                # post-run renderer → summary.txt
    │
    ├── hooks/
    │   ├── __init__.py
    │   ├── registry.py               # HookSet, insertion-ordered firing
    │   └── builtins.py               # fail_evaluator_at, count_events, assert_squash_after_n_edits, ...
    │
    ├── scenarios/
    │   ├── __init__.py               # explicit scenario registry (just `correctness_testing` this phase)
    │   ├── base.py                   # Scenario protocol + ScenarioContext + Composite
    │   └── correctness_testing.py    # this phase's only scenario — single composite that exercises
    │                                 # the framework end-to-end (entry → mission → episode → attempt
    │                                 # → planner/executor/evaluator submission → mission close).
    │                                 # All other scenarios are DEFERRED to the next phase (see §10).
    │
    ├── squad/
    │   ├── __init__.py
    │   ├── definitions.py            # entry_executor / planner / executor / evaluator AgentDefinitions
    │   ├── runner.py                 # MockSquadRunner — scenario-driven, emits StreamEvents, snapshots stores
    │   ├── prompt_inspector.py       # PromptInspection (moved from mock_agent_execution.py)
    │   └── sandbox_probe.py          # write/read/edit/shell/batch/conflict primitives
    │
    ├── stores.py                     # create_in_memory_task_center_stores
    ├── runner.py                     # run_scenario(scenario, instance, sandbox_id, hooks=, audit_dir=) → Report
    ├── fixtures.py                   # pytest fixtures (sandbox, workspace_reset, audit_dir, recorder)
    │
    └── tests/
        ├── __init__.py
        ├── conftest.py
        └── test_correctness.py        # this phase — exercises correctness_testing scenario
                                       # end-to-end. Per-feature regression tests
                                       # (test_episode_retry, test_batch_edit, ...) DEFERRED.
```

---

## 6. Reuse of existing infrastructure

| Existing module | What it does | How the framework uses it |
|---|---|---|
| `message/stream_events.py` (`StreamEvent` family) | Typed events with `agent_name` + `run_id` for thinking, text, tool start/done, bg task | Carry agent-level events. Bridged into the audit log via the existing `on_agent_event` callback (already standard on `start_task_center_entry_run`). Real-agent runs get the same wiring for free. |
| `message/agent_message_recorder.py` (`AgentMessageJsonlRecorder`) | Append-only JSONL keyed on `(agent_name, run_id)`, flushes thinking/text on message boundaries | Reused as the writer for per-Task `message.jsonl`. **Flushes mid-run** on every message boundary — no buffering until end. Wrapped with a primary-role allowlist so helper/subagent invocations skip the file system entirely. |
| `prompt/message_recorder.py` (`append_prompt_report_event`) | Atomic `os.write(O_APPEND)` JSONL writer with `ts` stamp | Bedrock for every `*.jsonl` we add: `mission.jsonl`, `episode.jsonl`, `attempt.jsonl`, `task.jsonl`. |
| `prompt/prompt_report_recorder.py` (`PromptReportRecorder`) | Adds monotonic `seq` + `base_event` merging | Pattern copied for the lifecycle snapshot writers. |
| `task_center.api::start_task_center_entry_run(..., on_agent_event=...)` | Allows external observers of the task-center run | Hooked to the in-memory `AuditEventBus`, which fans out to the LifecycleObserver, scenario hooks, and metrics aggregator. **No event log persisted to disk.** |
| `db.stores.{mission_store,episode_store,attempt_store,task_center_store}` | CRUD for the four ORM records | The LifecycleObserver fetches the fresh row from the store on each `*_UPDATED` event and serializes it into the corresponding `*.jsonl`. Single source of truth — the audit file mirrors the row. |
| `task_center.context_engine.recipes.{entry_executor,planner,generator,evaluator}` | Build `ContextPacket` for each role | Read-only during the run. The renderer's output is captured into per-agent `context.txt`. |

The mock squad currently passes `on_agent_event=None` and uses an internal `_noop_emit`. The framework hijacks both: agent emissions feed the in-memory bus (driving hooks + metrics) **and** the `AgentMessageJsonlRecorder` (writing per-Task `message.jsonl` mid-run, gated by the primary-role allowlist). The four ORM-mirror `*.jsonl` files are written separately by SQLAlchemy commit listeners — see §7.6.

---

## 7. Persistence layout (canonical)

```
.sweevo_runs/                                # gitignored, override via EOS_SWEEVO_AUDIT_DIR
└── scenario_logs/
    └── <scenario_name>/                     # e.g. episode_retry, batch_edit, full_mission_smoke
        └── <task_center_run_id>/            # format: 20260508T141207Z_<short_hash>
            ├── run.json                     # rewritten on status changes (start, finish, abort)
            ├── metrics.json                 # post-rendered tool/sandbox metrics at run end
            │
            ├── entry_executor_<task_id>/    # SIBLING of mission — entry executor's task
            │   ├── task.jsonl               # APPEND-ONLY, one line per Task row update
            │   └── message.jsonl            # APPEND-ONLY mid-run, one line per ConversationMessage
            │
            └── mission_01_<mission_id>/
                ├── mission.jsonl            # APPEND-ONLY, one line per MissionRecord update
                │
                ├── episode_01_<episode_id>/
                │   ├── episode.jsonl        # APPEND-ONLY, one line per EpisodeRecord update
                │   │
                │   ├── attempt_01_<attempt_id>/
                │   │   ├── attempt.jsonl    # APPEND-ONLY, one line per AttemptRecord update
                │   │   ├── 01_planner_<task_id>/
                │   │   │   ├── task.jsonl
                │   │   │   └── message.jsonl
                │   │   ├── 02_executor_<task_id>/
                │   │   │   ├── task.jsonl
                │   │   │   └── message.jsonl
                │   │   └── 03_evaluator_<task_id>/
                │   │       ├── task.jsonl
                │   │       └── message.jsonl
                │   │
                │   └── attempt_02_<attempt_id>/...
                │
                ├── episode_02_<episode_id>/...
                │
                └── mission_02_<sub_mission_id>/   # nested sub-mission via request_mission_solution
                    └── ...same shape recursively...
```

### 7.1 Naming rules

- Numeric prefix `NN_` on every ordered child directory — `mission_01_`, `episode_01_`, `attempt_01_`, `01_planner_`, `02_executor_`, `03_evaluator_`. Lexical sort = chronological.
- Directory names: `<order>_<role_or_kind>_<id>`. Greppable by role (`grep -r evaluator`) and by ID.
- Per-agent dir is keyed on the **TaskCenterTaskRecord id** — one Task row maps to one dir. Helpers/subagents that don't have a Task row get no directory.
- `<task_center_run_id>` format: `<UTC_ISO_timestamp>_<short_hash>` so listings sort chronologically.
- Composite scenarios use joined names: `full_mission_smoke+episode_retry+batch_edit`.

### 7.2 Per-Task directory (formerly per-agent)

Each per-Task directory holds **exactly two files**:

| File | Contents | Write trigger |
|---|---|---|
| `task.jsonl` | `_serialize_task(TaskCenterTaskRecord)` — one line per `upsert_task` call. Carries `system_prompt` + `user_prompt` + `summaries` + `status` directly, so the rendered prompts are inline (no separate `context.txt`). | Every store update to the Task row. |
| `message.jsonl` | One `ConversationMessage` per line — system / user(=user_prompt) / assistant(with thinking + tool_use) / user(tool_result). Same wire shape as Claude API; matches `message/messages.py::ConversationMessage`. | **Mid-run, on every message boundary** (each `assistant` reply, each `tool_result`). The existing `AgentMessageJsonlRecorder` already flushes per boundary — we just route its output here. |

**Helper / subagent filter:** the recorder is gated by an allowlist of primary roles — `{"entry_executor","planner","executor","evaluator"}`. Agent invocations outside that set (helper classifiers, summarizers, internal validators) get **no** directory and **no** `message.jsonl`. The bus still observes them, but they are not persisted.

`task.jsonl` schema (one line per update; last line = current state):
```jsonl
{"ts":1715170325.001,"row":{"id":"task_planner_01","role":"planner","status":"running","system_prompt":"...","user_prompt":"...","summaries":[],"task_input":"...","needs":[],"task_center_attempt_id":"att_1","context_packet_id":"ctx_1","agent_name":"planner_v1",...}}
{"ts":1715170325.500,"row":{...,"status":"done","summaries":[{"kind":"full_plan",...}]}}
```

`message.jsonl` schema:
```jsonl
{"ts":1715170325.123,"role":"system","content":[{"type":"text","text":"<system prompt — same as task row's system_prompt>"}]}
{"ts":1715170325.345,"role":"user","content":[{"type":"text","text":"<rendered context — same as task row's user_prompt>"}]}
{"ts":1715170326.789,"role":"assistant","content":[{"type":"thinking","text":"..."},{"type":"tool_use","id":"toolu_1","name":"submit_partial_plan","input":{...}}]}
{"ts":1715170327.001,"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"...","is_error":false}]}
```

For the **mock squad**, the `assistant` turn is synthesized from the deterministic tool call — same on-disk shape as a real run, so one parser handles both.

### 7.3 Per-lifecycle files (append-only ORM snapshots)

Each lifecycle directory holds **one append-only JSONL** named after the kind:
- `mission_NN_<id>/mission.jsonl`
- `episode_NN_<id>/episode.jsonl`
- `attempt_NN_<id>/attempt.jsonl`

**Each line = one full ORM row snapshot** at the time of an update. Trigger: the LifecycleObserver subscribes to the in-memory bus's `MISSION_*` / `EPISODE_*` / `ATTEMPT_*` events, fetches the fresh ORM record from its store, serializes it, and appends one line. **Read the last line for current state; read all lines for the mutation history.**

There is **no `phase` field** — the diff between consecutive lines tells you what changed (e.g., `episode_ids` grew, `status` flipped to `succeeded`, `task_specification` was set at close).

#### Mission row snapshot (`MissionRecord` columns)

```json
{
  "ts": 1715170325.001,
  "row": {
    "id": "...",
    "task_center_run_id": "...",
    "requested_by_task_id": "...",
    "goal": "...",
    "status": "open|succeeded|failed|cancelled",
    "episode_ids": ["..."],
    "final_outcome": null,
    "created_at": "...", "updated_at": "...", "closed_at": null,
    "context": null,
    "summary": null
  }
}
```

#### Episode row snapshot (`EpisodeRecord` columns)

```json
{
  "ts": 1715170325.500,
  "row": {
    "id": "...",
    "mission_id": "...",
    "sequence_no": 1,
    "creation_reason": "initial|partial_continuation",
    "goal": "...",
    "attempt_budget": 3,
    "status": "open|succeeded|failed|cancelled",
    "attempt_ids": ["..."],
    "continuation_goal": null,
    "task_specification": null,             // denorm at close from passing attempt
    "task_summary": null,                   // ditto
    "context": null,
    "summary": null,
    "created_at": "...", "updated_at": "...", "closed_at": null
  }
}
```

#### Attempt row snapshot (`AttemptRecord` columns)

```json
{
  "ts": 1715170326.000,
  "row": {
    "id": "...",
    "episode_id": "...",
    "attempt_sequence_no": 1,
    "stage": "planning|generating|evaluating|closed",
    "status": "running|passed|failed",
    "planner_task_id": "...",
    "task_specification": null,
    "evaluation_criteria": [],
    "generator_task_ids": [],
    "evaluator_task_id": null,
    "continuation_goal": null,
    "fail_reason": null,
    "created_at": "...", "updated_at": "...", "closed_at": null,
    "context": null,
    "summary": null
  }
}
```

#### Task row snapshot (`TaskCenterTaskRecord` columns — written under per-Task dir)

See §7.2. The Task `task.jsonl` is the same kind of file — one append-only line per `upsert_task`, carrying the full row including `system_prompt` and `user_prompt`.

### 7.4 Top-level files

| File | Contents | Write pattern |
|---|---|---|
| `run.json` | `{task_center_run_id, instance_id, sandbox_id, scenarios, hooks, started_ts, finished_ts, status}` | Atomic rewrite (`tmp + os.replace`) at run start, on each terminal status flip, and at run end. Small file, infrequent updates. |
| `metrics.json` | Aggregations: per-tool latency p50/p95/total, count per tool, layer growth, squash count. | Atomic rewrite once at run end, from in-memory aggregates the bus accumulated. |

(`events.jsonl`, `summary.txt`, and `tasks.jsonl` are gone — events live in-memory only; summary is a derived view of `mission.jsonl` + `metrics.json` produced ad-hoc by a `tree`-style helper if needed; per-Task `task.jsonl` files replace the global `tasks.jsonl`.)

### 7.5 Atomic writes

| File kind | Mechanism |
|---|---|
| `*.jsonl` (mission/episode/attempt/task/message) | `prompt.message_recorder.append_prompt_report_event` — `os.open(O_WRONLY\|O_CREAT\|O_APPEND) + os.write + os.close`. POSIX guarantees atomic append for writes ≤ `PIPE_BUF`; our records are well under that. Crash-resilient, concurrent-safe, no tmp/rename. |
| `run.json`, `metrics.json` | Write to `<file>.tmp` → `os.replace(<file>.tmp, <file>)`. POSIX-atomic rename. Updates are infrequent, so tmp+rename cost is negligible. |

### 7.6 Write triggers

| File | Creation trigger | Update trigger | Mechanism |
|---|---|---|---|
| `mission.jsonl` | First append (lazy — `O_CREAT \| O_APPEND` + `mkdir -p` parent) | Every commit to `MissionRecord` (insert or update) | `sqlalchemy.event.listens_for(MissionRecord, "after_insert" / "after_update")` — appends one line with the fresh row |
| `episode.jsonl` | Same | Every commit to `EpisodeRecord` | Same — `after_insert` / `after_update` on `EpisodeRecord` |
| `attempt.jsonl` | Same | Every commit to `AttemptRecord` | Same — `after_insert` / `after_update` on `AttemptRecord` |
| `task.jsonl` | Same | Every commit to `TaskCenterTaskRecord` (i.e., every `upsert_task` call) | Same — `after_insert` / `after_update` on `TaskCenterTaskRecord` |
| `message.jsonl` | First append (lazy) | Every `ConversationMessage` boundary mid-run (each `assistant` reply, each `tool_result`) | Existing `AgentMessageJsonlRecorder` flushes on message boundary — wired through the `on_agent_event` callback already used by `start_task_center_entry_run`. Gated by primary-role allowlist. |
| `run.json` | Explicit write at run start | Status change (running → finished / aborted) | Atomic `tmp + os.replace` |
| `metrics.json` | Explicit write at run end | Run end only | Atomic `tmp + os.replace`, from in-memory aggregates the bus accumulated |

#### Why ORM event listeners, not the in-memory event bus

The ORM-mirror `.jsonl` files must fire on **every committed row change**, regardless of which code path caused it.

- **SQLAlchemy `after_insert` / `after_update` listeners** (the chosen mechanism) fire automatically on commit. Production code doesn't need to emit anything. The audit recorder registers four listeners at bootstrap and stays neutral. An update from a backfill script, a test, or production all produce the same audit line.
- **In-memory event bus** would require every mutation site to remember to publish a `MISSION_UPDATED` event. Easy to miss; couples production code to the audit layer.

The in-memory bus stays in scope for **non-ORM events** — scenario hooks, metrics aggregation, sandbox-derived signals (`SANDBOX_LAYER_GROWN`, `SANDBOX_SQUASH_TRIGGERED`), agent-stream events. But the file-write trigger for the four lifecycle JSONL files is the ORM commit, not the bus.

#### Directory creation

Lazy. When the recorder first writes `mission_01_<id>/mission.jsonl`, the writer `mkdir -p`s the parent. The numeric prefix (`mission_01_`, `episode_02_`, ...) is computed by reading the parent directory's existing child count *at that moment* and adding one — so concurrent inserts under the same parent are handled by the listener serialization the SQLAlchemy session already enforces (one commit at a time per session).

#### Per-Task directory placement

`task.jsonl` is written under the per-Task directory whose path is determined by the Task's role + parent context:

- `role == "entry_executor"` → `<run_id>/entry_executor_<task_id>/`
- `role in {"planner","generator","evaluator"}` → `<run_id>/mission_NN_<mission_id>/episode_NN_<episode_id>/attempt_NN_<attempt_id>/<NN>_<role>_<task_id>/` — the parent attempt is read from `task_center_attempt_id`, and mission/episode are walked back via the attempt's `episode_id` and the episode's `mission_id`.
- Anything else (helpers/subagents) → no directory; line skipped.

The path resolver is a pure function of the row + a few store lookups; it's deterministic and idempotent (re-running the listener for the same row yields the same path).

### 7.7 Retention

`.sweevo_runs/` added to `.gitignore`. CLI helper:
```
python -m benchmarks.sweevo.live_test.audit.prune --keep 5 [--scenario X]
```
Keeps the latest N runs per scenario.

---

## 8. Event model

```python
# audit/events.py
class EventType(StrEnum):
    # task center lifecycle
    RUN_STARTED, RUN_COMPLETED
    MISSION_STARTED, MISSION_COMPLETED, MISSION_REQUESTED   # nested submissions
    EPISODE_STARTED, EPISODE_COMPLETED, EPISODE_CONTINUATION_CREATED
    ATTEMPT_STARTED, ATTEMPT_PASSED, ATTEMPT_FAILED

    # agent invocations
    PLANNER_INVOKED, PLANNER_FULL_PLAN, PLANNER_PARTIAL_PLAN, PLANNER_REPLAN
    EXECUTOR_INVOKED, EXECUTOR_SUCCESS, EXECUTOR_FAILURE
    EVALUATOR_INVOKED, EVALUATOR_SUCCESS, EVALUATOR_FAILURE

    # tools
    TOOL_CALL_STARTED, TOOL_CALL_COMPLETED, TOOL_CALL_ERROR

    # sandbox-derived (from tool result metadata or read-back probe)
    SANDBOX_WRITE_COMMITTED, SANDBOX_EDIT_COMMITTED, SANDBOX_SHELL_COMMITTED
    SANDBOX_BATCH_EDIT_APPLIED, SANDBOX_CONFLICT_DETECTED
    SANDBOX_LAYER_GROWN, SANDBOX_SQUASH_TRIGGERED

    # hook synthetic
    HOOK_INJECTED_FAILURE, HOOK_ASSERTED


@dataclass(frozen=True, slots=True)
class Event:
    ts: datetime
    type: EventType
    node: NodeId                 # hierarchical breadcrumb
    payload: dict[str, Any]      # type-specific, schema'd per event type
    correlation_id: str | None   # ties pre/post events for the same operation
```

### NodeId — the hierarchical breadcrumb

```python
@dataclass(frozen=True, slots=True)
class NodeId:
    task_center_run_id: str
    mission_id: str | None = None
    mission_seq: int | None = None      # sub-missions get incrementing seqs
    episode_id: str | None = None
    episode_seq: int | None = None
    attempt_id: str | None = None
    attempt_seq: int | None = None
    agent_role: Literal["entry_executor","planner","executor","evaluator"] | None = None
    agent_name: str | None = None
    agent_run_id: str | None = None
    tool_name: str | None = None
```

Every emitted event carries the most-specific `NodeId` available at emission time. **Events live in-memory only** — they drive the LifecycleObserver (which writes per-entity `*.jsonl`), the HookSet, and the metrics aggregator. There is no persisted `events.jsonl`. The on-disk audit tree is rebuilt by walking the directory structure (each Mission/Episode/Attempt/Task has its own dir + `*.jsonl`).

---

## 9. Hook framework

```python
# hooks/registry.py
@dataclass(frozen=True, slots=True)
class Hook:
    name: str
    event: EventType
    when: Literal["pre", "post"]
    fn: Callable[[Event, MutableMockState], HookResult]

class HookSet:
    def register(self, hook: Hook) -> None: ...
    def fire(self, event: Event, when: Literal["pre","post"], state) -> Iterator[HookResult]: ...
```

Hooks see the live event and a `MutableMockState` handle that lets them:
- `state.inject_failure(role="evaluator", attempt_id=...)` — next role-handler call returns failure
- `state.replace_next_planner_response(spec)` — swap planner output
- `state.assert_event_sequence([...])` — record an assertion outcome
- Just observe / record (e.g., metrics)

**Insertion order** is the firing order. Multiple hooks for the same event fire in registration order.

### Built-in hooks (`hooks/builtins.py`)

| Hook | Purpose |
|---|---|
| `count_events(event_type)` | Returns running count |
| `fail_evaluator_at(attempt_seq=1)` | Reusable retry trigger |
| `assert_squash_after_n_edits(n=16)` | Counts `SANDBOX_LAYER_GROWN`, then waits for `SANDBOX_SQUASH_TRIGGERED`, asserts ordering |
| `capture_prompt(role)` | Pulls `prompt_preview` for later assertion |
| `assert_event_sequence(expected: list[EventPattern])` | Declarative ordering check |

---

## 10. Scenario protocol

```python
# scenarios/base.py
class Scenario(Protocol):
    name: str
    expected_event_sequence: tuple[EventPattern, ...]   # for declarative assertion

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec | None: ...
    def executor_actions(self, ctx: ScenarioContext) -> Sequence[ToolCallSpec]: ...
    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec | None: ...
    def hooks(self) -> Sequence[Hook]: ...

    @classmethod
    def compose(cls, *parts: "Scenario") -> "Scenario": ...
```

`ScenarioContext` carries: `attempt`, `episode`, `mission`, `prompt`, `metadata`, `audit_recorder`, `mutable_state`. Scenarios are pure description; the runner translates them into actual tool calls.

### Hello-world scenario

```python
class BatchEdit(Scenario):
    name = "batch_edit"
    expected_event_sequence = (
        EventPattern(EventType.PLANNER_FULL_PLAN),
        EventPattern(EventType.EXECUTOR_INVOKED),
        EventPattern(EventType.SANDBOX_BATCH_EDIT_APPLIED, payload={"applied_edits": 2}),
        EventPattern(EventType.EVALUATOR_SUCCESS),
        EventPattern(EventType.MISSION_COMPLETED),
    )

    def planner_response(self, ctx):
        return ToolCallSpec(submit_full_plan, full_plan_with_one_task("batch_edit_probe"))

    def executor_actions(self, ctx):
        path = ".ephemeralos/sweevo-mock/batch.txt"
        return [
            ToolCallSpec(write_file_tool, {"file_path": path, "content": "alpha\nbeta\n"}),
            ApiCallSpec(sandbox_api.edit_file, EditFileRequest(
                path=ctx.absolute(path),
                edits=(SearchReplaceEdit("alpha\n", "alpha-batch\n"),
                       SearchReplaceEdit("beta\n", "beta-batch\n")),
                description="batch edit",
            )),
            ToolCallSpec(submit_execution_success, {"summary": "batch edit applied"}),
        ]
```

The runner figures out the rest: agent definitions, prompt inspection, tool dispatch, audit emission, hook firing.

### Scenario catalog

**This phase ships only `correctness_testing` plus `base.py`.** Every other scenario is deferred to a follow-up phase. Reason: the framework infrastructure (audit, hooks, scenarios protocol, squad runner, ORM commit listeners) is the load-bearing change for this phase; one composite correctness scenario is enough to prove it works end-to-end. Per-feature regression scenarios are valuable but additive — they can be authored independently once the framework is green.

| Scenario | Phase | Verifies |
|---|---|---|
| `correctness_testing` | **this phase** | One composite scenario exercising the full happy-path and one failure path: entry → mission → episode → attempt 1 (planner full plan + executor success + evaluator success) → mission close. Asserts every ORM `*.jsonl` round-trips, every per-Task `message.jsonl` is mid-run-flushed with the expected `system_prompt` + `user_prompt`, helper agents are filtered out, hooks fire in insertion order. This is the regression baseline the framework guarantees. |
| `full_mission_smoke` | next phase | entry → mission → episode → attempt → done; no failures |
| `episode_retry` | next phase | evaluator fails attempt 1 → attempt 2 created with failed-attempt evidence in planner context |
| `episode_continuation` | next phase | partial plan with `continuation_goal` → next episode created; `previous_episode_results` block in continuation planner prompt |
| `nested_mission` | next phase | executor calls `request_mission_solution` mid-attempt → sub-mission directory under parent mission |
| `squash_after_n_edits` | next phase | fire N edits, await `SANDBOX_SQUASH_TRIGGERED`, verify integrity |
| `batch_edit` | next phase | `sandbox_api.edit_file` with N>1 edits → `applied_edits==N`, status committed |
| `conflict_detection` | next phase | edit with non-matching `old_text` → success=False, `conflict_reason` set, file unchanged |
| `direct_vs_gated_merge` | next phase | shell-write file + public-write file; both readable; ordering preserved |
| `layer_stack_integrity` | next phase | mutate via direct + gated; final read after squash boundary contains every layer |
| `prompt_context_only` | next phase | no-op tool calls; assert only on `PromptInspection` for each role at each attempt/episode position |

---

## 11. Architecture diagram

Two independent write paths: ORM commits drive lifecycle `*.jsonl`; agent message boundaries drive `message.jsonl`. The in-memory bus is observability only — it does NOT drive file writes for the four ORM-mirror files.

```
              ┌────────────────────────────────────┐
              │  start_task_center_entry_run(...)   │
              └─────────────────┬──────────────────┘
                                │
                production code paths (squad runner, store writers, agent loop)
                                │
        ┌───────────────────────┼─────────────────────────────┐
        ▼                       ▼                             ▼
┌──────────────────┐   ┌────────────────────┐   ┌────────────────────────────┐
│  Stores commit   │   │  Agent emits       │   │  Scenario / sandbox / hook │
│  ORM rows:       │   │  ConversationMsg   │   │  events (in-memory only)   │
│  Mission/Episode/│   │  boundary          │   │                            │
│  Attempt/Task    │   │                    │   │                            │
└────────┬─────────┘   └─────────┬──────────┘   └──────────────┬─────────────┘
         │                       │                             │
         │ SQLAlchemy            │ on_agent_event              │ AuditEventBus
         │ after_insert /        │ callback                    │ (in-memory)
         │ after_update          │                             │
         ▼                       ▼                             ▼
┌──────────────────┐   ┌────────────────────────┐   ┌──────────────────────┐
│ LifecycleListener│   │ AgentMessageJsonl      │   │ HookSet +            │
│ (4 listeners,    │   │ Recorder               │   │ MetricsAggregator +  │
│  one per Record) │   │ (mid-run, primary-role │   │ LifecycleObserver    │
│                  │   │  allowlist only)       │   │ (read-only fanout)   │
└────────┬─────────┘   └─────────┬──────────────┘   └──────────────────────┘
         │                       │
         │ append_prompt_report_event (O_APPEND, atomic)
         ▼                       ▼
┌─────────────────────────────────────────────────────┐
│ Per-entity append-only files                         │
│   <run_dir>/mission_NN_*/mission.jsonl               │
│   <run_dir>/.../episode_NN_*/episode.jsonl           │
│   <run_dir>/.../attempt_NN_*/attempt.jsonl           │
│   <run_dir>/.../<NN>_<role>_<task_id>/task.jsonl     │
│   <run_dir>/.../<NN>_<role>_<task_id>/message.jsonl  │
└─────────────────────────────────────────────────────┘

  run start  ──→  run.json   (atomic tmp + os.replace)
  run end    ──→  metrics.json (atomic tmp + os.replace, from in-memory aggregates)
```

**Key invariants:**
- The four ORM-mirror `*.jsonl` files have exactly one writer path: SQLAlchemy commit listeners. Production code is unaware.
- `message.jsonl` is the only file whose write trigger lives inside the agent loop (mid-run boundaries).
- The in-memory `AuditEventBus` carries hooks, metrics, and scenario events. **No persisted `events.jsonl`.**

---

## 12. Pytest fixtures

```python
# live_test/fixtures.py
@pytest.fixture(scope="session")
def sweevo_instance():
    return select_sweevo_instance(
        instance_id=os.getenv("EOS_SWEEVO_INSTANCE", "dask__dask_2023.3.2_2023.4.0")
    )

@pytest.fixture(scope="session")
async def sweevo_sandbox(sweevo_instance):
    bootstrap_daytona_provider()
    return await create_sweevo_test_sandbox(sweevo_instance, register_snapshot=True)

@pytest.fixture
async def workspace(sweevo_sandbox, sweevo_instance, request):
    # Per decision (2): if the sandbox was just provisioned (first test), no reset needed.
    # Subsequent tests get git reset --hard HEAD && git clean -fd && rm -rf .ephemeralos/sweevo-mock.
    cache_key = "sweevo_sandbox_used"
    if request.session.config.cache.get(cache_key, False):
        await reset_sweevo_workspace(sweevo_sandbox["sandbox_id"])
    else:
        request.session.config.cache.set(cache_key, True)
    return sweevo_sandbox

@pytest.fixture
def audit_dir(request) -> Path:
    if os.getenv("EOS_SWEEVO_AUDIT_TMP") == "1":
        return request.getfixturevalue("tmp_path") / "sweevo_run"
    override = os.getenv("EOS_SWEEVO_AUDIT_DIR")
    base = Path(override) if override else Path(".sweevo_runs")
    return base.resolve()

@pytest.fixture
def stores() -> Iterator[TaskCenterStoreBundle]:
    bundle = create_in_memory_task_center_stores()
    try:
        yield bundle
    finally:
        bundle.close()
```

### Typical test

```python
# tests/test_episode_retry.py
@pytest.mark.asyncio
async def test_evaluator_failure_creates_attempt_2(
    sweevo_instance, workspace, audit_dir, stores,
):
    scenario = Scenario.compose(FullMissionSmoke(), EpisodeRetry(fail_at_attempt=1))
    report = await run_scenario(
        scenario,
        instance=sweevo_instance,
        sandbox_id=workspace["sandbox_id"],
        audit_dir=audit_dir,
        stores=stores,
    )
    assert_event_sequence(report.events, scenario.expected_event_sequence)
    attempts = report.lifecycle.missions[0].episodes[0].attempts
    assert [a.status for a in attempts] == ["failed", "passed"]
    assert (report.run_dir / "attempt_01_" + attempts[0].id / "attempt.jsonl").exists()
```

---

## 13. Squash detection

Two paths, depending on what `sandbox.api` exposes (resolved in step 0 spike):

1. **Tool-result metadata path (preferred):** `sandbox_api.write_file/edit_file/shell` results carry a `metadata.layer_count` and/or `squash_triggered` flag. The runner reads each result, emits `SANDBOX_LAYER_GROWN` and (when present) `SANDBOX_SQUASH_TRIGGERED`. No sandbox internals imported.
2. **Read-back probe path (fallback):** after each mutating tool call the runner calls a lightweight read-back probe (e.g., `sandbox_api.get_workspace_status`) to count layers. Same emitted events, slightly more wire chatter.

`squash_after_n_edits` scenario then asserts:
- `count(SANDBOX_LAYER_GROWN) >= n`
- exactly one `SANDBOX_SQUASH_TRIGGERED` between edit `n` and edit `n+1`
- final `read_file` after squash returns the cumulative content

---

## 14. Tier wiring

Append to `backend/tests/live_e2e_test/_tools/tiers.toml`:

```toml
[[tier]]
id = 7
name = "sweevo_mock_framework"
wall_budget_s = 600        # ~90s sandbox bring-up + ~30s/test * ~12 tests
per_cell_budget_s = 60
kind = "pytest"
pytest_args = ["backend/src/benchmarks/sweevo/live_test/tests/", "-q"]
cascade = "warn"
```

---

## 15. Migration steps (each independently shippable)

| # | Task | Notes |
|---|---|---|
| 0 | **Spike: squash event surface** | Read `sandbox.api.write_file/edit_file/shell` result types. Confirm whether tool-result metadata exposes layer count or squash signal. Lock in metadata vs probe path. Document choice in `live_test/audit/squash_detection.md`. ~30 min. |
| 1 | **Extract `prompt.py`** | Move `build_sweevo_user_prompt`, `pr_description_for_instance`, `load_pr_description_overrides` from `task_center_runner.py` into `src/benchmarks/sweevo/prompt.py`. Update one import. Tests stay green. |
| 2 | **Skeleton `live_test/`** | Create directory tree, empty `__init__.py`s, stubs for `events.py`/`node_id.py`/`recorder.py`. |
| 3 | **Move squad + stores** | Relocate `MockSWEvoAgentExecution` → `live_test/squad/runner.py` (split out prompt inspector + sandbox probe). `create_in_memory_task_center_stores` → `live_test/stores.py`. |
| 4 | **Wire EventBus** | Squad emits events at agent invocation, tool call started/completed, attempt/episode lifecycle (read from stores after each squad call), terminal results. Add `NodeId` breadcrumb to every event. |
| 5 | **Implement AuditRecorder** | `events.jsonl` + per-agent `context.txt` + `message.jsonl` + per-lifecycle append-only `*.jsonl`; render `summary.txt` + `metrics.json` post-run. |
| 6 | **Scenario + Hook protocols** | `live_test/scenarios/base.py` + `live_test/hooks/registry.py`. Port the existing preflight + integrity + continuation flow into 3 scenarios so behavior is preserved as a regression. |
| 7 | **Author scenarios + tests (this phase)** | Ship `scenarios/base.py` (Scenario protocol + ScenarioContext + Composite) and `scenarios/correctness_testing.py` (single composite scenario covering happy-path + one failure path). One pytest file: `tests/test_correctness.py`. All other scenarios from §10 (`full_mission_smoke`, `episode_retry`, `episode_continuation`, `nested_mission`, `squash_after_n_edits`, `batch_edit`, `conflict_detection`, `direct_vs_gated_merge`, `layer_stack_integrity`, `prompt_context_only`) are **deferred to the next phase**. |
| 8 | **Pytest fixtures** | `fixtures.py`, `tests/conftest.py`. |
| 9 | **Update legacy unit test** | `backend/tests/unit_test/test_benchmarks/test_sweevo_mock_agent_execution.py` → point imports at `live_test/`, keep `_FakeSandboxApi` for the no-Daytona regression. Same scenarios should run under the fake. |
| 10 | **Slim `__main__.py`** | Keep only `--real-agent` and `--scenario <name>` ad-hoc smoke. Pytest is the canonical mock entry. |
| 11 | **Tier wiring** | Update `tiers.toml` (tier 7 above). Re-run `python -m backend.tests.live_e2e_test._tools.run_tiered --tier 7` to validate end-to-end. |
| 12 | **Delete legacy** | Remove `src/benchmarks/sweevo/mock_agent_execution.py` and `src/benchmarks/sweevo/task_center_runner.py` once all imports are migrated. |

---

## 16. Recipe ↔ persistence cross-reference

This locks in the data captured at each level so the directory structure is grounded in the existing TaskCenter contracts.

| Level | DTO source | Recipe input read at this level | Persistence file | Snapshot phases |
|---|---|---|---|---|
| Mission | `task_center.mission.mission.Mission` | `goal` (planner mission_goal block, ep≥2); `requested_by_task_id` (cross-ref to entry/parent task) | `mission_NN_<id>/mission.jsonl` | created, episode_added, succeeded, failed, cancelled |
| Episode | `task_center.episode.episode.Episode` | `goal` (episode_goal block); `task_specification` + `task_summary` denorm at close (prior_episode_specification/summary blocks); `creation_reason`; `continuation_goal` | `episode_NN_<id>/episode.jsonl` | created, attempt_added, task_specification_set, task_summary_set, succeeded, failed, cancelled |
| Attempt | `task_center.attempt.state.Attempt` | `task_specification` (planner output, generator+evaluator input); `evaluation_criteria` (evaluator input); `generator_task_ids` (evaluator completed_task_summary); `continuation_goal` (signals next episode) | `attempt_NN_<id>/attempt.jsonl` | created, planner_submitted, generator_added, generator_completed, evaluator_assigned, passed, failed |
| Entry executor | (no DTO; AgentDefinition `entry_executor`) | `entry_executor_v1` recipe → entry_request block from task_input | `entry_executor_<agent_run_id>/{context.txt, message.jsonl}` | n/a |
| Planner | (no DTO; AgentDefinition `planner`) | `planner_v1` recipe → mission/episode frame + failed_attempt_landscape | `NN_planner_<agent_run_id>/{context.txt, message.jsonl}` | n/a |
| Executor | (no DTO; AgentDefinition `executor`) | `generator_v1` recipe → task_specification + dependency_summary + planned_task_spec | `NN_executor_<agent_run_id>/{context.txt, message.jsonl}` | n/a |
| Evaluator | (no DTO; AgentDefinition `evaluator`) | `evaluator_v1` recipe → mission/episode frame + task_specification + completed_task_summary + evaluation_criteria | `NN_evaluator_<agent_run_id>/{context.txt, message.jsonl}` | n/a |

---

## 17. Open implementation question

Resolved in step 0 spike, then locked:

- **Sandbox API surface for layer count / squash trigger.** Are layer counts surfaced via tool-result `metadata`, or do we need a read-back probe? This determines whether `squash_after_n_edits` is implementable without sandbox-internal imports.

All other decisions are locked above.

---

## 18. Acceptance

This plan is acceptable when:

1. **Framework infrastructure exists** — `live_test/{audit,hooks,scenarios,squad,stores,runner,fixtures}` ships per §5.
2. **`scenarios/base.py` + `scenarios/correctness_testing.py` exist** and are wired into the registry. All other scenarios listed in §10 are explicitly marked as next-phase work.
3. **ORM extensions are in place** — `MissionRecord` / `EpisodeRecord` / `AttemptRecord` carry `context` + `summary`; `TaskCenterTaskRecord` carries `system_prompt` + `user_prompt`; the matching domain DTOs (`Mission`/`Episode`/`Attempt`) gained `context` + `summary`. Alembic migration deployed.
4. **Triggers are wired per §7.6** — SQLAlchemy `after_insert`/`after_update` listeners drive the four lifecycle `*.jsonl`; `AgentMessageJsonlRecorder` (mid-run, primary-role-allowlist) drives `message.jsonl`; `run.json` and `metrics.json` are written via atomic `tmp + os.replace`.
5. **`test_correctness.py` passes** end-to-end against a real Daytona sandbox + real TaskCenter runtime + the deterministic mock squad, exercising entry → mission → episode → attempt → submission → close. The on-disk run dir reflects every ORM commit (one `.jsonl` line each) and every message boundary (one `message.jsonl` line each).
6. **Migration steps in §15 are independently mergeable** — each step leaves the codebase green.

Once accepted, execution starts at step 0. The deferred scenarios in §10 become the first phase after this one.

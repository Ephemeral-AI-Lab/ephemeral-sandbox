# Implementation Plan: Verifier Specialist & Replanning

## Design Overview

**One tool, one replanning path:**

1. **`request_replan()`** — available to ALL agents (specialists + verifier). "I can't finish / tests are failing, here's context" → persists a replan artifact and terminates self. The engine handles everything else.
2. **Verifier specialist** — pure test runner spawned as the **last child of each expandable node**. If tests fail, it calls `request_replan()` with structured test results. No special tools, no DAG knowledge needed.
3. **`update_plan`** — posthook tool for the replanner agent, analogous to `submit_plan_tasks` but scoped to **mutating an existing running plan** at the current node level. Cannot touch completed tasks.

The replanning path: `request_replan()` persists artifact + terminates self → `_cascade_and_finalize()` detects artifact → engine cancels siblings + spawns replanner → replanner calls `update_plan` → engine applies mutations → re-dispatch.

---

## Why Unified Replanning Is Better

### Previous design (two tools: `escalate_task()` + `replan_node()`):
- Two separate mutation code paths — one for specialists, one for verifier
- Verifier needs task planning skills (`task-decompose`) + DAG awareness
- `replan_node()` does direct graph mutation from inside a running agent — complex concurrency
- Two tools to maintain, test, and document

### This design (one tool: `request_replan()` for everyone):
- **Single code path**: all replanning flows through the same tool → engine handler → replanner agent → `update_plan`
- **Verifier stays simple**: pure test runner → if fail → `request_replan(reason, context)` → done
- **No agent-side graph mutation**: agents only signal; engine acts
- **Clean separation**: tool is thin (persist + raise), engine owns all side effects (cancel siblings, spawn replanner)
- **Easier to test**: one tool, one engine handler, one mutation path

---

## Part A: `request_replan()` — Universal Replanning Tool

### Semantics

Any agent (specialist or verifier) calls `request_replan()` when it **cannot complete** its task or detects issues that require replanning.

```python
request_replan(
    reason: str,           # Why this task can't be completed / what failed
    context: str,          # What was discovered, partial progress, test output, errors
    suggestion: str = "",  # Optional: what the agent thinks should happen next
)
```

**For a normal specialist**, this means:
- "Scope mismatch — the function I need to change is in a different module"
- "Circular dependency — can't fix A without fixing B first, but B depends on A"
- "Missing prerequisite — need a config change before this code change works"

**For the verifier**, this means:
- "3/10 FAIL_TO_PASS tests still failing — here are the test IDs and error messages"
- "2 PASS_TO_PASS regressions introduced — here are the details"

### Effect

The tool is **thin** — it only does two things:

1. Persist replan payload as task artifact
2. Raise `ReplanRequested` to terminate the calling agent

All other side effects (sibling cancellation, replanner spawn) happen **engine-side** in `_cascade_and_finalize()` after the task terminates.

### Tool Implementation

```python
@tool(name="request_replan")
def request_replan(reason: str, context: str, suggestion: str = "") -> str:
    """Request replanning for this node.

    This is a TERMINAL action — calling this ends your task immediately.
    The coordination engine will cancel sibling tasks, spawn a replanner
    agent, and emit replacement tasks based on your reason and context.

    Args:
        reason: Why this task can't be completed (or what tests failed)
        context: Detailed context — partial progress, error output, test results
        suggestion: Optional hint for the replanner about what should happen next
    """
    replan_payload = {
        "type": "replan_request",
        "reason": reason,
        "context": context,
        "suggestion": suggestion,
        "task_id": _bound_task_id,
        "run_id": _bound_run_id,
    }

    # 1. Persist as task artifact
    _bound_store.update_task_artifact(
        _bound_run_id, _bound_task_id,
        artifact={"replan_request": replan_payload},
    )

    # 2. Terminal — agent runtime stops after this
    #    Engine handles sibling cancellation + replanner spawn in _cascade_and_finalize()
    raise ReplanRequested(replan_payload)
```

### Where It Lives

New toolkit: `backend/src/toolkits/coordination_worker_toolkit/__init__.py`

Added to specialist definitions via `"toolkits": ["daytona_tools", "coordination_worker"]`. The tool is injected at dispatch time with closures over `task_id`, `run_id`, `store` — same pattern as existing worker hooks.

---

## Part B: Verifier Specialist — Pure Test Runner

### Design: Simple Agent, No DAG Skills

The verifier is a **pure test runner**. It doesn't need to understand the task graph, call `plan_tasks()`, or know about DAG structure. It just:

1. Runs scoped FAIL_TO_PASS tests
2. Runs relevant PASS_TO_PASS tests
3. If all pass → completes successfully
4. If failures → calls `request_replan()` with structured test results

The **engine** handles all replanning logic based on the replan payload.

```json
{
    "kind": "agent",
    "name": "verifier-sweevo",
    "description": "Runs scoped FAIL_TO_PASS and PASS_TO_PASS test suites to verify specialist implementation results. Reports failures via request_replan().",
    "instructions": [
        "You are a test verification agent for SWE-EVO benchmark tasks.",
        "CONDA ENVIRONMENT (CRITICAL): Before running ANY Python command, always activate: `. /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed`.",
        "WORKFLOW: 1) Run the FAIL_TO_PASS tests listed in your task description. 2) Run relevant PASS_TO_PASS tests to check for regressions. 3) Report results.",
        "IF ALL TESTS PASS: Complete successfully with a summary of test results.",
        "IF TESTS FAIL: Call request_replan() with reason='test_failures' and context containing: the failed test IDs, error messages, and which sibling tasks likely caused the failures based on test file paths.",
        "Do NOT modify any source files. Your job is verification only.",
        "Format your replan context as structured text: list each failed test ID on its own line with the error summary, then a blank line, then your analysis of which implementation areas need rework."
    ],
    "tools": [],
    "toolkits": ["daytona_tools", "coordination_worker"],
    "skills": ["shared-sandbox-guardrails"],
    "model_key": "specialist-light"
}
```

**Key properties:**
- No `coordination` toolkit (no DAG tools)
- No task planning skills (`task-decompose`, `coordination-runtime-basics`)
- Uses `"specialist-light"` model key (cheaper — mostly test execution + structured reporting)
- Just `daytona_tools` (sandbox access) + `coordination_worker` (replan tool)

### Verifier as Last Child of Each Expandable Node

```
Root Plan
├── expand-dataframe (expandable)
│   ├── fix-core-api (worker)
│   ├── fix-io-layer (worker)
│   └── verify-dataframe (verifier) ← depends_on: [fix-core-api, fix-io-layer]
├── expand-cli (expandable)
│   ├── fix-cli-register (worker)
│   └── verify-cli (verifier) ← depends_on: [fix-cli-register]
└── verify-root (verifier) ← depends_on: [expand-dataframe, expand-cli]
```

Each node's verifier runs the **subset** of tests relevant to that node's scope (derived from `ci_plan.touches_paths` of sibling tasks). Root verifier catches cross-node regressions.

### Verifier Injection

Programmatic — injected after plan extraction, not reliant on planner.

**In expansion** (`expansion.py`):
```python
# After planned_run = extract_plan(expansion_run_id, store=store)
if planned_run and planned_run.tasks:
    _inject_node_verifier(planned_run, parent_task=task)
    handoff_planned_run(planned_run, ...)
```

**At root** (`orchestration.py`):
```python
# After plan = extract_plan(run_id, store=store)
if plan and plan.tasks:
    _inject_node_verifier(plan, parent_task=None)
    handoff_planned_run(plan, ...)
```

```python
def _inject_node_verifier(
    planned_run: CoordinationPlan,
    parent_task: TeamTask | None,
) -> None:
    all_leaf_ids = [
        tid for tid, t in planned_run.tasks.items()
        if not t.expandable
    ]
    if not all_leaf_ids:
        return

    node_label = parent_task.task_id if parent_task else "root"
    verifier_id = f"verify-{node_label}"
    # Collect test scope from sibling tasks' touches_paths
    scoped_paths = []
    for tid in all_leaf_ids:
        scoped_paths.extend(planned_run.tasks[tid].ci_plan.touches_paths)

    verifier = TeamTask(
        task_id=verifier_id,
        description=(
            f"Run FAIL_TO_PASS and PASS_TO_PASS tests scoped to: {', '.join(scoped_paths[:10])}. "
            f"If failures, request replan with structured test results."
        ),
        agent_name="verifier-sweevo",
        depends_on=all_leaf_ids,
        task_type=TaskType.WORKER,
        ci_plan=TaskCIPlan(touches_paths=["__verification__"]),
    )
    planned_run.tasks[verifier_id] = verifier
```

---

## Part C: `update_plan` — Declarative Plan Mutation Tool

### Design: `submit_plan_tasks` for Running Plans

`update_plan` is a posthook tool for the replanner agent. It mirrors `submit_plan_tasks` but operates on an **existing running plan** with constraints that protect completed work.

```python
update_plan(
    add_tasks: list[TaskDef],        # New tasks to insert (status=PENDING)
    cancel_task_ids: list[str] = [], # Pending-only tasks to remove
)
```

### Validation Rules

1. **`cancel_task_ids`** must all reference tasks with `status == PENDING`. Reject if any task is `RUNNING` or `COMPLETED`.
2. **`add_tasks`** dependencies must reference:
   - Existing `COMPLETED` tasks, OR
   - Other tasks in `add_tasks` (new-to-new deps)
   - NOT `RUNNING`, `FAILED`, or `CANCELLED` tasks
3. **No cycles** in the resulting graph (same validation as `submit_plan_tasks`)
4. **Agent name validation** — same roster check as initial planning
5. **Verifier auto-reset**: if a verifier task exists and is `FAILED`, reset it to `PENDING` with deps on the new tasks

### Engine-Side Application

```python
def _apply_plan_update(
    plan: CoordinationPlan,
    *,
    add_tasks: list[TeamTask],
    cancel_task_ids: list[str],
    store: Any,
    run_lock: threading.Lock,
) -> None:
    """Validate and atomically apply plan mutations under lock."""
    with run_lock:
        # 1. Validate cancel targets are PENDING
        for tid in cancel_task_ids:
            task = plan.tasks.get(tid)
            if task is None or task.status != TaskStatus.PENDING:
                raise ValueError(f"Cannot cancel task {tid}: not PENDING")

        # 2. Validate new task deps reference COMPLETED or other new tasks
        new_task_ids = {t.task_id for t in add_tasks}
        for task in add_tasks:
            for dep in task.depends_on:
                if dep in new_task_ids:
                    continue
                existing = plan.tasks.get(dep)
                if existing is None or existing.status != TaskStatus.COMPLETED:
                    raise ValueError(
                        f"New task {task.task_id} depends on {dep} "
                        f"which is not COMPLETED"
                    )

        # 3. Cycle check on merged graph
        _validate_no_cycles(plan, add_tasks, cancel_task_ids)

        # 4. Apply cancellations
        for tid in cancel_task_ids:
            plan.tasks[tid].status = TaskStatus.CANCELLED
            store.compare_and_update_task_status(
                plan.plan_id, tid,
                expected_status=TaskStatus.PENDING,
                new_status=TaskStatus.CANCELLED,
            )

        # 5. Add new tasks to store + in-memory plan
        if add_tasks:
            store.add_tasks_to_running_plan(plan.plan_id, [
                {
                    "task_id": t.task_id,
                    "description": t.description,
                    "agent_name": t.agent_name,
                    "depends_on": t.depends_on,
                    "expandable": t.expandable,
                    "expansion_hint": t.expansion_hint,
                    "touches_paths": t.ci_plan.touches_paths,
                }
                for t in add_tasks
            ])
            for task in add_tasks:
                plan.tasks[task.task_id] = task

        # 6. Reset verifier if present and FAILED
        _reset_verifier(plan, add_tasks, store)


def _reset_verifier(
    plan: CoordinationPlan,
    new_tasks: list[TeamTask],
    store: Any,
) -> None:
    """Reset FAILED verifier to PENDING with deps on new tasks."""
    for task in plan.tasks.values():
        if task.agent_name == "verifier-sweevo" and task.status == TaskStatus.FAILED:
            new_task_ids = [t.task_id for t in new_tasks]
            task.status = TaskStatus.PENDING
            task.error = None
            # Keep deps on completed tasks, drop cancelled, add new
            task.depends_on = [
                tid for tid in task.depends_on
                if plan.tasks.get(tid) and plan.tasks[tid].status != TaskStatus.CANCELLED
            ] + new_task_ids
            store.compare_and_update_task_status(
                plan.plan_id, task.task_id,
                expected_status=TaskStatus.FAILED,
                new_status=TaskStatus.PENDING,
            )
            # Clear old artifact (clean slate for re-run)
            store.update_task_artifact(plan.plan_id, task.task_id, artifact=None)
            break  # One verifier per node
```

---

## Part D: Engine Replan Handler + Replanner Agent

### Engine-Side: `_cascade_and_finalize()` Replan Detection

After a task terminates with a `replan_request` artifact, `_cascade_and_finalize()` detects it and orchestrates the replan:

```python
def _cascade_and_finalize(
    plan: CoordinationPlan,
    *,
    ctx: RunContext,
) -> None:
    store = ctx.store
    store_active = store is not None and store.is_available
    run_lock = ctx.get_run_lock(plan.plan_id)
    with run_lock:
        _block_failed_dependency_tasks(
            plan,
            coordination_store=store if store_active else None,
        )

        # NEW: Check for replan request before normal dispatch/finalize
        if _should_replan(plan, store):
            _cancel_sibling_tasks(plan, store=store, cancel_tasks_fn=ctx.cancel_tasks_fn)
            store.update_run_metadata(plan.plan_id, {"replanned": True})
            ctx.bridge.schedule_coroutine(
                _run_replanner(plan, ctx=ctx),
                name=f"replan-{plan.plan_id}",
                track_fn=ctx.track_fn,
            )
            return  # Don't finalize — replanner will re-dispatch

        ctx.cascade_dispatch_fn(plan, ctx=ctx)
        final_run_status = ctx.finalize_run_fn(plan, ctx=ctx)
        if final_run_status:
            logger.info(
                "Coordination run %s finished with status=%s",
                plan.plan_id,
                final_run_status,
            )


def _should_replan(plan: CoordinationPlan, store: Any) -> bool:
    """Check if a replan was requested and hasn't been handled yet."""
    if not store or not store.is_available:
        return False

    # Single-attempt guard
    metadata = store.get_run_metadata(plan.plan_id) or {}
    if metadata.get("replanned"):
        return False

    # Check for replan_request artifact on any failed task
    for task in plan.tasks.values():
        if task.status == TaskStatus.FAILED:
            artifact = _load_task_artifact(store, plan.plan_id, task.task_id)
            if artifact and "replan_request" in artifact:
                return True

    return False


def _cancel_sibling_tasks(
    plan: CoordinationPlan,
    *,
    store: Any,
    cancel_tasks_fn: Callable[[str], int],
) -> None:
    """Cancel all non-terminal sibling tasks in the node."""
    error_msg = "Cancelled: replan requested"
    for task in plan.tasks.values():
        if task.status == TaskStatus.QUEUED:
            store.compare_and_update_task_status(
                plan.plan_id, task.task_id,
                expected_status=TaskStatus.QUEUED,
                new_status=TaskStatus.CANCELLED,
                error=error_msg,
            )
            task.status = TaskStatus.CANCELLED
        elif task.status == TaskStatus.RUNNING:
            store.compare_and_update_task_status(
                plan.plan_id, task.task_id,
                expected_status=TaskStatus.RUNNING,
                new_status=TaskStatus.FAILED,
                error=error_msg,
            )
            task.status = TaskStatus.FAILED
    # Cancel asyncio tasks for the run (terminates agent runtimes)
    cancel_tasks_fn(plan.plan_id)
```

### Replanner Agent

The replanner is spawned by the engine after sibling cancellation. It receives:

```python
replan_context = {
    "replan_request": replan_payload,  # reason, context, suggestion from the requesting agent
    "task_graph": {                    # Current node state
        "completed": [{"task_id": ..., "summary": ..., "artifact": ...}],
        "failed": [{"task_id": ..., "error": ...}],
        "cancelled": [{"task_id": ..., "error": ...}],
    },
}
```

What it does:
1. Analyzes the replan reason + completed work
2. Decides what new tasks are needed to address the issue
3. Calls `update_plan(add_tasks=[...], cancel_task_ids=[...])` as its posthook
4. Engine validates + applies mutations atomically
5. Engine re-dispatches via `cascade_dispatch()`

```python
async def _run_replanner(
    plan: CoordinationPlan,
    *,
    ctx: RunContext,
) -> None:
    """Spawn a replanner agent that calls update_plan to mutate the DAG."""
    store = ctx.store

    # Collect replan request from the failed task that triggered this
    replan_payload = None
    for task in plan.tasks.values():
        if task.status == TaskStatus.FAILED:
            artifact = _load_task_artifact(store, plan.plan_id, task.task_id)
            if artifact and "replan_request" in artifact:
                replan_payload = artifact["replan_request"]
                break

    if replan_payload is None:
        return

    # Build context from current node state
    replan_context = {
        "replan_request": replan_payload,
        "task_graph": _summarize_task_graph(plan, store),
    }

    # Spawn replanner agent — it calls update_plan as its posthook
    await _run_replanner_agent(plan, replan_context, ctx=ctx)

    # Re-dispatch under lock
    run_lock = ctx.get_run_lock(plan.plan_id)
    with run_lock:
        ctx.cascade_dispatch_fn(plan, ctx=ctx)
```

---

## Part E: Artifact Semantics & Loop Prevention

### Artifact Overwrite

Each task run **overwrites** the entire artifact (clean slate). When the verifier is reset to `PENDING` and re-runs:
- Its old artifact is cleared during `_reset_verifier()`
- On re-run, if tests pass → completed artifact (no replan key)
- On re-run, if tests fail → new replan artifact written

No merge semantics. Overwrite is the simplest model.

### Loop Prevention

Single boolean `replanned` per node in run metadata:

```
Replan flow (first time):
  agent calls request_replan() → persists artifact → self terminates
  → _cascade_and_finalize() detects replan_request + replanned == False
  → engine cancels siblings, sets replanned = True
  → engine spawns replanner
  → replanner calls update_plan, new tasks added, verifier reset
  → new tasks run → verifier re-dispatched
  → pass → node done

Replan flow (second time):
  agent calls request_replan() again → persists artifact → self terminates
  → _cascade_and_finalize() detects replan_request + replanned == True
  → skips replan, proceeds with normal failure path
  → node finalizes with failures
```

---

## Flow Diagram

```
Normal specialist flow:
  specialist runs → succeeds → cascade → dispatch next → finalize when done

Replan flow (specialist or verifier):
  agent calls request_replan(reason, context)
    → artifact persisted on task
    → ReplanRequested raised → task fails
    → _process_terminal_outcome() fires normally
    → _cascade_and_finalize():
        → detects replan_request artifact + replanned == False
        → cancels all sibling RUNNING/QUEUED tasks
        → sets replanned = True
        → spawns replanner agent
    → replanner analyzes replan context + completed work
    → replanner calls update_plan(add_tasks=[...])
    → engine validates + applies mutations atomically
    → verifier reset to PENDING with new deps
    → cascade_dispatch() runs new tasks
    → verifier re-runs after new tasks complete
    → pass → node done | fail + replanned == True → node finalizes with failures

Verifier flow:
  all sibling tasks complete → verifier dispatched
    → runs scoped FAIL_TO_PASS + PASS_TO_PASS tests
    → if all pass → completes → node finalizes
    → if failures → request_replan(reason="test_failures", context=structured_results)
      → same replan flow as above (one attempt)
```

---

## Implementation Steps

### Step 1: `request_replan()` Tool + `ReplanRequested` Exception
- **Files**:
  - `backend/src/toolkits/coordination_worker_toolkit/__init__.py` (NEW) — toolkit with `request_replan()` bound at dispatch time with closures over task_id, run_id, store
  - `backend/src/services/coordination/core/models.py` (MODIFY) — add `ReplanRequested(Exception)` class

### Step 2: Wire `ReplanRequested` into Worker Dispatch
- **File**: `backend/src/services/coordination/engine/dispatch.py` (MODIFY)
- **Description**: In `_run_worker_task()`, catch `ReplanRequested` and route to `_process_terminal_outcome()` with status=FAILED. The artifact is already persisted by the tool.

### Step 3: `update_plan` Posthook Tool
- **File**: `backend/src/services/coordination/planning/workflow/phase_hooks.py` (MODIFY)
- **Description**: Register `update_plan` posthook tool. Mirrors `submit_plan_tasks` validation (graph cycles, agent roster) plus mutation constraints: cancel only PENDING, deps only on COMPLETED or new tasks. Calls `_apply_plan_update()` atomically.

### Step 4: Engine Replan Handler in `_cascade_and_finalize()`
- **File**: `backend/src/services/coordination/engine/worker_hooks.py` (MODIFY)
- **Description**: Add `_should_replan()` check between `_block_failed_dependency_tasks()` and `cascade_dispatch()`. On match: cancel siblings via `_cancel_sibling_tasks()`, set `replanned = True`, spawn replanner via `ctx.bridge.schedule_coroutine()`.

### Step 5: Store — `add_tasks_to_running_plan()`
- **Files**:
  - `backend/src/db/relational_db/coordination_store/task_store.py` (MODIFY)
  - `backend/src/services/coordination/infrastructure/store.py` (MODIFY)
- **Description**: Insert new tasks into a running plan. Increment total_tasks counter.

### Step 6: Verifier Specialist Definition
- **File**: `.super-cocoa-agents/specialist/verifier-sweevo.json` (NEW)
- **Description**: Pure test runner with `daytona_tools` + `coordination_worker` toolkits. Uses `request_replan()` to report failures. No planning skills. `specialist-light` model key.

### Step 7: Verifier Injection (Expansion + Root)
- **Files**:
  - `backend/src/services/coordination/engine/expansion/expansion.py` (MODIFY)
  - `backend/src/services/coordination/adapters/benchmark/sweevo_adapter/orchestration.py` (MODIFY)
- **Description**: After `extract_plan()`, inject a verifier task as the last child with `depends_on` all leaf task IDs.

### Step 8: Tests
- **File**: `backend/tests/test_request_replan.py` (NEW) — tool behavior, artifact persistence, terminal semantics
- **File**: `backend/tests/test_update_plan.py` (NEW) — mutation validation, PENDING-only cancel, dep constraints, verifier reset
- **File**: `backend/tests/test_replan_handler.py` (NEW) — `_should_replan()`, sibling cancellation, single-attempt guard, replanner spawn
- **File**: `backend/tests/test_verifier_injection.py` (NEW) — injection at expansion + root, dep wiring

---

## Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `backend/src/toolkits/coordination_worker_toolkit/__init__.py` | Create | `request_replan()` tool (thin: persist + raise) |
| `.super-cocoa-agents/specialist/verifier-sweevo.json` | Create | Pure test runner verifier |
| `backend/src/services/coordination/core/models.py` | Modify | `ReplanRequested` exception |
| `backend/src/services/coordination/engine/dispatch.py` | Modify | Catch `ReplanRequested` in worker runner |
| `backend/src/services/coordination/engine/worker_hooks.py` | Modify | `_should_replan()` + `_cancel_sibling_tasks()` + replanner spawn in `_cascade_and_finalize()` |
| `backend/src/services/coordination/planning/workflow/phase_hooks.py` | Modify | `update_plan` posthook tool + `_apply_plan_update()` |
| `backend/src/db/relational_db/coordination_store/task_store.py` | Modify | `add_tasks_to_running_plan()` |
| `backend/src/services/coordination/infrastructure/store.py` | Modify | Mirror new store method |
| `backend/src/services/coordination/engine/expansion/expansion.py` | Modify | Inject verifier as last child |
| `backend/src/services/coordination/adapters/benchmark/sweevo_adapter/orchestration.py` | Modify | Inject root verifier |

---

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| Sibling cancellation races with in-flight completion | CAS on task status — if task already completed, CAS fails silently, completed work preserved |
| Task graph inconsistency during mutation | Hold `run_lock` during `_apply_plan_update()`; CAS on task status |
| Replanner agent crashes | `replanned = True` already set — node fails cleanly via normal `_cascade_and_finalize()` path |
| New tasks depend on non-COMPLETED tasks | `_apply_plan_update()` validates deps before applying |
| Verifier artifact confusion across runs | Artifact overwritten (clean slate) on each task run; cleared on reset |
| Verifier model cost at every node | Uses `"specialist-light"` model key |
| Replanner produces bad tasks | Same graph validation as `submit_plan_tasks` (cycles, agent roster, deps) |
| Infinite replan loops | Single boolean `replanned` per node — one attempt only |
| Multiple tasks fail with replan_request simultaneously | First `_cascade_and_finalize()` invocation wins (sets `replanned = True`); subsequent invocations see `replanned == True` and proceed normally |

---

## SESSION_ID
- CODEX_SESSION: N/A
- GEMINI_SESSION: N/A

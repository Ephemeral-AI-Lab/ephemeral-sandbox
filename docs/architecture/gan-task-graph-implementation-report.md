# GAN-Style Task Graph: Implementation Report

**Spec:** [`gan-task-graph-v1.md`](./gan-task-graph-v1.md)
**Implemented:** 2026-04-27

---

## 1. Summary of the Architectural Shift

The task graph moved from an **executor-evaluator tree with implicit closure chains** to an **explicit GAN-style harness graph** that separates planning, execution, and evaluation into three first-class roles, each with three-way terminal choices: success, soft failure, or escalate-to-planner.

```
BEFORE                                  AFTER
──────                                  ─────
executor (plans + runs work)            executor (runs work or escalates)
   │   submit_plan_handoff                 │   launch_plan_handoff
   ▼                                       ▼
[ children ] ───► evaluator             planner (read-only decomposition)
              submit_continue_work          │   submit_plan_handoff
              (only success/continue)       ▼
                                        [ children ] + evaluator
                                                      │
                                                      ▼
                                        success | hard-fail | recovery-plan
```

Decompositions are now bundled into a `TaskCenterHarnessGraph` — one planner, an executor DAG, and a single evaluator gate. Failure is explicit and bounded: `submit_task_failure` is scoped to one executor + dependency-blocked descendants, while `submit_evaluation_failure` closes the whole harness graph as failed and propagates through nested graph parents.

---

## 2. Workflow Diagrams

### 2.1 Simple Task (no decomposition)

```
┌────────────────────────┐
│ root executor (READY)  │
└───────────┬────────────┘
            │ submit_task_success(summary)
            ▼
┌────────────────────────┐
│ root executor (DONE)   │
│ summaries=[success]    │
└────────────────────────┘
```

### 2.2 Plan-Driven Happy Path

```
root executor                     planner P                    children + evaluator
─────────────                     ─────────                    ────────────────────
[RUNNING]
  │ launch_plan_handoff
  │  (creates harness graph H, planner P)
  ▼
[HANDOFF]
                                  [READY]
                                    │ submit_plan_handoff
                                    │  (materializes children + evaluator E in H)
                                    ▼
                                  [HANDOFF]
                                                                a [READY]
                                                                b [PENDING needs={a}]
                                                                E [PENDING needs={sinks}]
                                                                  │ a → DONE → b → DONE
                                                                  ▼
                                                                E [READY]
                                                                  │ submit_task_success
                                                                  ▼
                                                                E [DONE]
                                  ◄────── close_harness_graph_success ───────
                                  [DONE]
[DONE] ◄──── child_success summary ────
```

### 2.3 Soft Fail (executor fails, evaluator chooses)

```
plan: a, b deps=[a], c
  ┌─ a [RUNNING] ──submit_task_failure─► [FAILED] (summary=failure)
  │       │
  │       └─► dependency_blocked_descendants → b [PENDING] → [FAILED] (summary=dependency_blocked)
  │
  ├─ c [RUNNING] ──submit_task_success──► [DONE]   (summary=success)
  │
  └─ all executors terminal → evaluator dispatched
        │
        ├─ submit_task_success("partial ok")            → graph closes successfully
        ├─ submit_evaluation_failure("can't recover")   → graph closes failed
        └─ launch_plan_handoff("fix the gap")           → spawns recovery planner
```

### 2.4 Hard Fail (evaluator hard-fails the graph)

```
evaluator E ──submit_evaluation_failure─► [FAILED]
                          │
                          ▼
              close_harness_graph_failed
                          │
                          ├─► planner P                  → [FAILED]
                          ├─► parent_task                → [FAILED] + child_failure summary
                          └─► outer harness graph notified
                                  │
                                  ├─ if parent_task is outer-evaluator → recurse close
                                  └─ if parent_task is outer-executor  → outer evaluator wakes
```

### 2.5 Evaluator-Driven Replan (Recovery)

```
evaluator E (in graph H)
   │ launch_plan_handoff(task_detail="fix the gap")
   ▼
[E HANDOFF]
   │
   ▼
new harness graph H'    root_task_id = E
   │
   ▼
recovery planner P' input = PlannerLaunchContext{
   caller_role="evaluator",
   requested_goal=parent_goal(E),               # outer parent_task.input
   prior_planner_handoff=P.handoffs,            # the original plan
   completed_child_summaries=[...],             # what worked
   failed_child_summaries=[...],                # what didn't
   dependency_blocked_summaries=[...],          # what was stranded
   task_detail="fix the gap"                    # E's gap statement
}
   │ submit_plan_handoff(corrective_tasks)
   ▼
[children + recovery evaluator E']
   │
   ▼
on success: close H' → E becomes DONE → close H → original parent_task DONE
```

### 2.6 Nested Graph Recovery

```
outer graph H                    inner graph H'
  parent=root                      parent=x (executor in H)
                                   ─────────────────────────
  x [RUNNING]
    │ launch_plan_handoff
    ▼
  x [HANDOFF] ──────────────────► planner P' [READY]
                                     │ submit_plan_handoff
                                     ▼
                                  child y, evaluator E'
                                  E' submit_evaluation_failure
                                     │
                                     ▼
                                  close H' → P' FAILED, x FAILED + child_failure
  ◄────── notify_child_terminal_changed ────
  evaluator E [PENDING] now sees x FAILED;
  is_harness_graph_ready_for_evaluation(H) = True
    │
    ▼
  E [READY] → decides: success | hard-fail | re-plan
```

---

## 3. Data Model Changes

### 3.1 `Task` (renamed and slimmed)

```python
@dataclass
class Task:
    id: TaskId
    role: Literal["executor", "planner", "evaluator"]   # widened: + planner
    input: str                                          # renamed from `spec`
    status: Status
    task_center_harness_graph_id: HarnessGraphId | None # new: replaces parent_id/closes_for
    needs: frozenset[TaskId]
    summaries: list[TaskSummary]                        # new: was scalar `summary`
    created_at: float
```

**Removed:** `parent_id`, `closes_for`, scalar `summary`, `title`, `children`, `evaluator_id`, `acceptance_criteria`, `handoff_note`.

### 3.2 `TaskSummary` (new, append-only)

```python
@dataclass
class TaskSummary:
    kind: Literal[
        "handoff",            # planner submitted plan; caller initiated launch_plan_handoff
        "success",            # executor/evaluator submit_task_success
        "failure",            # executor submit_task_failure
        "evaluation_failure", # evaluator submit_evaluation_failure
        "dependency_blocked", # descendant FAILED because a transitive dep failed
        "child_success",      # graph closed successfully → parent_task receives this
        "child_failure",      # graph closed failed → parent_task receives this
    ]
    text: str
    source_task_id: TaskId
    created_at: float
```

### 3.3 `TaskCenterHarnessGraph` (new)

```python
@dataclass
class TaskCenterHarnessGraph:
    id: HarnessGraphId
    run_id: str
    root_task_id: TaskId       # the executor or evaluator that launched the planner
    planner_task_id: TaskId
    evaluator_task_id: TaskId | None     # populated by submit_plan_handoff
    executor_task_ids: list[TaskId]      # populated by submit_plan_handoff
```

### 3.4 `PlannerLaunchContext` (new, structured input for planner)

```python
@dataclass
class PlannerLaunchContext:
    task_detail: str
    caller_task_id: TaskId
    caller_role: Literal["executor", "evaluator"]
    requested_goal: str
    upstream_handoff_summaries: list[TaskSummary]
    prior_planner_handoff: list[TaskSummary]
    completed_child_summaries: list[TaskSummary]
    failed_child_summaries: list[TaskSummary]
    dependency_blocked_summaries: list[TaskSummary]
```

---

## 4. Mode-Tool Surface

| Tool | Caller role | Effect |
|------|-------------|--------|
| `submit_task_success(summary)` | executor or evaluator | Append `success`; mark DONE. For evaluator, close owning harness graph successfully. |
| `submit_task_failure(summary)` | executor only | Append `failure`; mark FAILED; mark dependency-blocked descendants FAILED. |
| `submit_evaluation_failure(summary)` | evaluator only | Append `evaluation_failure`; mark FAILED; close owning harness graph as failed. |
| `launch_plan_handoff(task_detail)` | executor or evaluator | Append `handoff`; caller → HANDOFF; build `PlannerLaunchContext`; create new harness graph + planner task. |
| `submit_plan_handoff(tasks, task_inputs, handoff_summary)` | planner only | Append `handoff`; planner → HANDOFF; materialize executor children (with `needs`) and an evaluator (`needs = sinks`) in the planner's harness graph. |

**Deleted:** `submit_task_completion` (renamed to `submit_task_success`), `submit_continue_work_handoff` (replaced by `launch_plan_handoff` from evaluator).

---

## 5. TaskCenter API

### 5.1 Mode-tool entry points

- `submit_task_success(task_id, summary)`
- `submit_task_failure(task_id, summary)`
- `submit_evaluation_failure(task_id, summary)`
- `launch_plan_handoff(task_id, task_detail)`
- `submit_plan_handoff(planner_id, tasks, task_inputs, handoff_summary)`

### 5.2 Graph helpers

- `parent_goal(task_id) -> str | None` — input of the harness graph's parent_task
- `planner_handoff(task_id) -> list[TaskSummary]` — handoff summaries from this graph's planner
- `completed_dependencies(task_id) -> list[Task]` — direct deps with status=DONE
- `failed_dependencies(task_id) -> list[Task]` — direct deps with status=FAILED
- `dependency_blocked_descendants(task_id) -> list[Task]` — non-terminal executor descendants whose dep path now contains the failed task
- `is_harness_graph_ready_for_evaluation(graph_id) -> bool` — True iff every executor child is DONE or FAILED

### 5.3 Closure propagation (replaces old `closes_for` chain)

- `_close_harness_graph_success(graph_id, source_task_id)` — planner DONE, append `child_success` to parent_task, parent DONE, recurse if parent is outer-evaluator.
- `_close_harness_graph_failed(graph_id, source_task_id)` — planner FAILED, append `child_failure` to parent_task, parent FAILED, recurse if parent is outer-evaluator.
- `_propagate_parent_terminal(parent, success)` — bubbles closure to outer graph: re-close if parent is outer-evaluator; otherwise just notify wakeup so outer evaluator dispatch can pick up the new terminal state.
- `_notify_child_terminal_changed(graph_id)` — sets the dispatcher wakeup; the loop polls `is_harness_graph_ready_for_evaluation` and promotes evaluators from PENDING to READY.

### 5.4 Dispatcher

- `_promote_ready_evaluators()` — for each harness graph, if all executor children are terminal (DONE or FAILED) and the evaluator is PENDING, transition to READY. This handles partial-failure cases where `ready_tasks()` alone wouldn't promote (because `needs` requires DONE).
- `ready_tasks()` (in `TaskGraph`) — pure dependency-only logic; does **not** consult parent task state, harness graph state, or role.
- `_run_one(task_id, sandbox_id)` — runs the spawn function; on silent exit, treats it as the role's failure terminal (executor → `submit_task_failure`, evaluator → `submit_evaluation_failure`, planner → close graph as failed).

---

## 6. New / Modified Files

### 6.1 New files

| Path | Purpose |
|------|---------|
| `backend/src/task_center/model/harness.py` | `HarnessGraph` dataclass |
| `backend/src/task_center/harness_agents/planner/context.py` | `PlannerLaunchContext` dataclass + renderer |
| `backend/src/task_center/harness_agents/{executor,evaluator}/context.py` | Executor/evaluator dispatch contexts |
| `backend/src/task_center/graph/dag.py` | Graph-owned DAG validation, sink calculation, and ID collision helpers |
| `backend/src/tools/mode_tool/submit_task_success.py` | Renamed terminal tool |
| `backend/src/tools/mode_tool/submit_task_failure.py` | New executor-only terminal |
| `backend/src/tools/mode_tool/submit_evaluation_failure.py` | New evaluator-only terminal |
| `backend/src/tools/mode_tool/launch_plan_handoff.py` | New executor/evaluator escalation |
| `docs/architecture/gan-task-graph-v1.md` | Specification |
| `docs/architecture/gan-task-graph-implementation-report.md` | This document |

### 6.2 Heavily modified files

| Path | Change |
|------|--------|
| `backend/src/task_center/model/task.py` | New `Task` shape + `TaskSummary` |
| `backend/src/task_center/graph/` | Stores harness graphs, DAG helpers, readiness, and graph queries |
| `backend/src/task_center/runtime/orchestrator.py` | Five mode-tool entry points delegate to role lifecycle modules; evaluator promotion based on graph readiness |
| `backend/src/task_center/__init__.py` | New exports |
| `backend/src/task_center/harness_agents/prompts.py` | Role-aware prompt context: planner gets pre-rendered context, executor sees deps, evaluator sees parent goal + planner handoff + child summaries |
| `backend/src/tools/mode_tool/__init__.py` | New tool registration |
| `backend/src/tools/mode_tool/_models.py` | Removed `TaskSpec` |
| `backend/src/tools/mode_tool/submit_plan_handoff.py` | Planner-only; signature changed to `tasks`, `task_inputs`, `handoff_summary` |
| `backend/src/task_center/harness_agents/{planner,executor,evaluator}/definition.py` | Role-local agent definitions loaded from sibling `agent.md` files |
| `backend/src/agents/builtins.py` | Registers role-local harness agents plus the explorer subagent |
| `backend/src/agents/types.py` | Default mode terminal renamed |
| `backend/src/db/models/task_center.py` | Reshaped: `TaskCenterHarnessGraphRecord` (one row per harness graph); `TaskCenterTaskRecord` carries `summaries`/`needs`/`task_center_harness_graph_id` |
| `backend/src/db/stores/task_center_store.py` | New `upsert_harness_graph` + `list_harness_graphs_for_run` |
| `backend/src/db/engine.py` | `_DROPPED_COLUMNS` map drops obsolete `title`/`summary` columns on startup |
| `backend/src/server/routers/persistence.py` | Endpoint returns `{"harness_graphs": ...}` |
| `backend/src/engine/testing/eval_agent.py` | Terminal renamed |

### 6.3 Deleted files

| Path | Reason |
|------|--------|
| `backend/src/task_center/propagation.py` | `closes_for` chain replaced by harness-graph closure |
| `backend/src/tools/mode_tool/submit_continue_work_handoff.py` | Replaced by `launch_plan_handoff` from evaluator |
| `backend/src/tools/mode_tool/submit_task_completion.py` | Renamed to `submit_task_success.py` |
| `backend/tests/test_task_center/test_propagation.py` | Tested deleted module |

---

## 7. Test Coverage

The 11 verification scenarios from the spec are covered by `backend/tests/test_task_center/test_center.py`:

| Scenario | Test |
|----------|------|
| Simple task success | `test_simple_task_success` |
| Simple task failure | `test_simple_task_failure` |
| Plan-driven happy path | `test_plan_driven_happy_path` |
| Soft fail | `test_soft_fail_dependency_blocked` |
| Hard fail | `test_hard_fail_propagates_to_root` |
| Nested graph recovery | `test_nested_graph_recovery` |
| Evaluator-driven replan | `test_evaluator_driven_replan_context` |
| Graph helpers | `test_graph_helpers` |
| Evaluator dispatch under partial failure | `test_evaluator_dispatch_under_partial_failure` |
| Role rejection | `test_role_rejection_both_directions` |
| Summary history | `test_summary_history_coexists` |

Plus DAG pipelining, silent-termination, sandbox passthrough, and fresh-graph-per-query bonus tests. Total: 686 backend tests pass; ruff clean.

---

## 8. Conclusion: Benefits of the Architectural Shift

### 8.1 Explicit failure terminal

Before, the only failure path was an agent crash or a silent exit, which the dispatcher mapped to a generic FAILED status. Soft failures (one branch unrecoverable, others fine) and hard failures (the whole goal cannot be met) looked identical from the graph's perspective. The new model makes failure a first-class decision: `submit_task_failure` is bounded to one branch and lets the evaluator decide recovery, while `submit_evaluation_failure` closes the whole harness graph. The graph topology itself encodes "this branch failed, but the goal might still be achievable."

### 8.2 Planning is a separable cognitive task

The old executor conflated three jobs — investigate, plan, execute — into one role with a 100-tool budget. The new model gives each cognitive task its own agent: planner (read-only investigation + DAG emission), executor (direct work + soft fail), evaluator (closure decision). Each agent has a smaller tool surface and a sharper terminal contract, which makes prompts shorter and behavior easier to audit.

### 8.3 Recovery paths are structured, not ad-hoc

The deleted `submit_continue_work_handoff` could only spawn a single continuation executor under the evaluator. The new `launch_plan_handoff` spawns a planner with a full structured `PlannerLaunchContext` — prior plan, what worked, what failed, what was stranded, the gap to repair. Recovery is decomposable, not just a "try once more" continuation. Nested escalations (executor → planner → child → planner → ...) are all expressible without growing the set of terminal tools.

### 8.4 Graph closure is local to the decomposition unit

The old `closes_for` chain threaded a hidden pointer through every leaf task; closure walked that chain bottom-up across multiple parent levels. The new `TaskCenterHarnessGraph` makes the unit of closure explicit: one planner + executor DAG + one evaluator close as a unit. Cross-graph propagation happens only through `root_task_id` on the harness graph, and the rules for what happens when a graph closes (planner DONE/FAILED, parent_task receives `child_success`/`child_failure`, outer graph notified) are written down in five short methods on `TaskCenter` instead of distributed across propagation walkers.

### 8.5 Append-only summary history

A scalar `summary: str | None` was overwritten on every state transition. With `summaries: list[TaskSummary]`, every notable event leaves a typed entry — `handoff`, `success`, `failure`, `evaluation_failure`, `dependency_blocked`, `child_success`, `child_failure`. The full lifecycle of a task is reconstructable from the summary list, which simplifies auditing, debugging, and downstream reasoning by other agents.

### 8.6 Partial-failure evaluation falls out for free

Because evaluator dispatch is gated on `is_harness_graph_ready_for_evaluation` (DONE or FAILED for every executor child) rather than on every child being DONE, the same dispatch path handles both all-pass and partial-failure cases. The evaluator sees DONE summaries, FAILED summaries, and `dependency_blocked` summaries, and decides what to do. There is no special "stranded pending" status to track.

### 8.7 Persistence is one-row-per-decomposition

The old `task_center_graph` table stored one row per task with parent/children/evaluator pointers and acceptance criteria/handoff notes. Reconstructing a decomposition required walking parent pointers and collating columns scattered across rows. The new `task_center_harness_graph` table stores one row per harness graph with `root_task_id`, `planner_task_id`, `evaluator_task_id`, and `executor_task_ids`. Querying "which tasks form one decomposition" is a single row lookup, not a graph traversal.

### 8.8 Smaller blast radius for executor changes

The executor-first stance is preserved — simple tasks still finish via `submit_task_success` without spawning a planner. But the executor no longer plans, so its prompt and tool surface are simpler; changes to planning logic only touch the planner agent and `submit_plan_handoff`. Conversely, changes to validation logic are isolated to the evaluator agent and `submit_evaluation_failure` / `submit_task_success(evaluator)`. The three roles can evolve independently.

---

## Known Follow-ups (not implemented)

- **Per-child completion contracts** — each generator boundary may eventually need a more precise contract than planner-level task inputs.
- **Replan depth cap** — nothing bounds `executor → planner → child → planner → ...` chains; lineage depth tracking on harness graphs would let `launch_plan_handoff` reject runaway recursion.
- **Handoff heuristics** — instrumenting escalations per task lineage would surface premature delegation patterns.

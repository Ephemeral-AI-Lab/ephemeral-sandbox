# GAN-style task graph: planner / executor / evaluator with explicit success + failure

## Context

Today's executor conflates plan + execute and lacks an explicit failure terminal. We are separating planning into a dedicated role and giving executor + evaluator each a three-way terminal choice: success, failure, or escalate-to-planner.

Every decomposition is represented by a `TaskCenterHarnessGraph`:

- one planner task
- one or more executor child tasks
- one evaluator task
- a `root_task_id` pointing at the executor or evaluator that launched the planner

Tasks no longer carry `parent_id` or `closes_for`. A task belongs to at most one harness graph through `task_center_harness_graph_id`; only the root executor has no harness graph. Dependencies between executor tasks are represented only by `needs`.

Executor-first stance is preserved: simple tasks finish via `submit_task_success` without spawning a planner.

Tools after this PR:

- `submit_task_completion` -> renamed to `submit_task_success`
- `submit_task_failure` (new, executor-only) - soft fail; only this node and dependency-blocked descendants are affected ` (new, evaluator-only) - hard fail; closes the harness graph as failed and propagates through graph parents
- `launch_plan_handoff` (new) - terminal that spawns a planner harness graph
- `submit_continue_work_handoff` deleted

## Role Contracts

| Role | Inputs | Allowed tools | Terminals |
|---|---|---|---|
| `executor` | Root: user's request. Otherwise: `input` assigned by the enclosing planner. Completed dependency summaries only. | `DIRECT_WORK_TOOLS` | `submit_task_success` · `launch_plan_handoff` · `submit_task_failure` |
| `planner` | Structured planner launch context. Every planner gets the same context shape: caller input, handoff task detail, requested/enclosing goal, upstream/prior planner handoff summaries, DONE child summaries, FAILED child summaries, and dependency-blocked summaries when available from the caller's enclosing harness graph. | `PLANNER_TOOLS` | `submit_plan_handoff[handoff_summary, tasks, task_inputs]` |
| `evaluator` | Parent task input + planner handoff summary + sibling child summaries (DONE and FAILED). | `DIRECT_WORK_TOOLS` | `submit_task_success` · `launch_plan_handoff` · `submit_evaluation_failure` |
| `explorer` | unchanged | unchanged | unchanged |

Splitting `submit_task_failure` from `submit_evaluation_failure` keeps the tool name aligned with graph effect: executor failure is scoped; evaluator failure closes the harness graph as failed.

## Data Model

### Task

`Task.spec` is renamed to `Task.input`.

```python
Task:
    id: TaskId
    role: Literal["executor", "planner", "evaluator"]
    input: str
    status: Status
    task_center_harness_graph_id: HarnessGraphId | None
    needs: frozenset[TaskId]
    summaries: list[TaskSummary]
```

Only root executor tasks have `task_center_harness_graph_id is None`. Non-root executor tasks do not know a parent task. They only know their direct data dependencies through `needs`.

### TaskSummary

Replace scalar `summary: str | None` with an append-only list.

```python
TaskSummary:
    kind: Literal[
        "handoff",
        "success",
        "failure",
        "evaluation_failure",
        "dependency_blocked",
        "child_success",
        "child_failure",
    ]
    text: str
    source_task_id: TaskId
    created_at: float
```

This avoids overwriting one semantic summary with another. A planner can append a `handoff` summary, an executor can append a `success` or `failure` summary, and graph closure can append `child_success` or `child_failure` summaries to the task that launched the harness graph.

### TaskCenterHarnessGraph

`task_center_graph` is renamed to `task_center_harness_graph`. Use `root_task_id` for the requested graph parent pointer so it is clear that the parent is a task, not another graph.

```python
TaskCenterHarnessGraph:
    id: HarnessGraphId
    run_id: str
    root_task_id: TaskId
    planner_task_id: TaskId
    evaluator_task_id: TaskId | None
    executor_task_ids: list[TaskId]
```

The root executor is not inside any harness graph. Every planner-created executor and evaluator belongs to the harness graph created by `launch_plan_handoff`.

## Terminal Tool Semantics

| Tool | Caller | Effect |
|---|---|---|
| `submit_task_success(summary)` | executor | Append `success`; mark executor DONE; notify the owning harness graph that child terminal state changed. |
| `submit_task_success(summary)` | evaluator | Append `success`; mark evaluator DONE; close the owning harness graph successfully. That marks the graph planner DONE, appends `child_success` to the graph's `root_task_id`, and marks that parent task DONE. |
| `submit_task_failure(summary)` | executor only | Append `failure`; mark executor FAILED. Mark dependency-blocked descendants FAILED with `dependency_blocked` summaries. Notify the owning harness graph that child terminal state changed. |
| `submit_evaluation_failure(summary)` | evaluator only | Append `evaluation_failure`; mark evaluator FAILED; close the owning harness graph as failed. That marks the graph planner FAILED, appends `child_failure` to the graph's `root_task_id`, and marks that parent task FAILED. |
| `launch_plan_handoff(task_detail)` | executor or evaluator | Append `handoff` with the task detail; mark caller HANDOFF; build a structured planner launch context from the caller role; create a `TaskCenterHarnessGraph(root_task_id=caller.id)` and a planner task inside it with `input=planner_launch_context(caller.id, task_detail)`. |
| `submit_plan_handoff(tasks, task_inputs, handoff_summary)` | planner only | Append `handoff`; materialize executor children and evaluator inside the planner's harness graph. |

When a harness graph closes and marks its `root_task_id` terminal, TaskCenter also notifies the parent task's owning harness graph. This is the replacement for `closes_for` propagation and is the mechanism that wakes outer evaluators in nested graphs.

### Planner Launch Context

`launch_plan_handoff(task_detail)` is the boundary between "this task cannot or should not continue directly" and "a planner should decompose the next phase." The tool must not pass only `caller.input` to the planner, because evaluator-driven recovery would lose the evidence that made recovery necessary.

TaskCenter builds the planner task input from a structured context:

```python
PlannerLaunchContext:
    task_detail: str
    caller_task_id: TaskId
    caller_role: Literal["executor", "evaluator"]
    caller_input: str
    requested_goal: str
    upstream_handoff_summaries: list[TaskSummary]
    prior_planner_handoff: list[TaskSummary]
    completed_child_summaries: list[TaskSummary]
    failed_child_summaries: list[TaskSummary]
    dependency_blocked_summaries: list[TaskSummary]
```

For any caller:

- `caller_input` is always the local input assigned to the executor or evaluator that requested planning.
- `requested_goal` is the enclosing harness graph's parent goal when the caller belongs to a harness graph; otherwise it is `caller.input` for the root executor.
- `upstream_handoff_summaries` and `prior_planner_handoff` come from the caller's owning harness graph, if any.
- `completed_child_summaries`, `failed_child_summaries`, and `dependency_blocked_summaries` come from the caller's owning harness graph, if any.
- `task_detail` is the caller's explanation of what the new planner should plan.

The recovery planner's contract is to plan only the missing corrective work. It must treat successful child summaries as completed constraints, failed child summaries as repair evidence, and dependency-blocked summaries as work that may need replacement if still relevant.

## Task Graph Shape

```
root executor
  task_center_harness_graph_id = None

caller task
  status = HANDOFF
  summaries += handoff
        |
        v
TaskCenterHarnessGraph H
  root_task_id = caller task
  planner_task_id = P
  evaluator_task_id = E

        P planner
        input = planner_launch_context(caller.id, task_detail)
        summaries += handoff
             |
             v
   executor child 1      executor child 2      evaluator E
   graph_id = H          graph_id = H          graph_id = H
   needs = {}            needs = {child 1}     needs = sinks of child DAG
```

The planner and evaluator get graph-level context. Executor children get their own `input` plus completed dependency summaries. They do not treat dependency tasks as parent tasks.

## Derived Graph Properties

These helpers are computed from `Task.task_center_harness_graph_id`, `TaskCenterHarnessGraph`, and `needs`.

| Property | Resolution |
|---|---|
| `parent_goal(task_id)` | Find the task's harness graph; return `parent_task.input`. |
| `planner_handoff(task_id)` | Find the task's harness graph; return planner summaries with `kind == "handoff"`. |
| `completed_dependencies(task_id)` | Return direct dependency tasks whose status is DONE plus their summaries. |
| `failed_dependencies(task_id)` | Return direct dependency tasks whose status is FAILED plus their summaries. |
| `dependency_blocked_descendants(task_id)` | Return nonterminal transitive dependents whose dependency path now contains a FAILED task. |
| `planner_launch_context(task_id, task_detail)` | Return the structured context passed to a new planner from graph topology. The context shape is the same for executor and evaluator callers; fields are populated from the caller's enclosing harness graph when one exists. |

Top-level executors have no harness graph, so `parent_goal` and `planner_handoff` return `None`.

## Readiness And Evaluator Dispatch

`ready_tasks()` promotes a PENDING task only when every direct dependency in `needs` is DONE.

No `pending_reason` field is needed. A task whose dependency path fails is marked FAILED with a `dependency_blocked` summary, so the graph does not need a special stranded-pending state.

`is_harness_graph_ready_for_evaluation(graph_id) -> bool` returns True iff every executor child in the harness graph is terminal:

- DONE
- FAILED

Equivalently: dispatch the evaluator when the harness graph has no unfinished executor tasks. This handles both all-pass and partial-failure cases with the same rule.

## Failure Semantics

### Executor Soft Fail

```
executor X submits submit_task_failure(summary)
        |
        v
TaskCenter.submit_task_failure(X):
    X.summaries += TaskSummary(kind="failure", text=summary, source_task_id=X.id)
    X.status = FAILED
    for D in dependency_blocked_descendants(X.id):
        D.summaries += TaskSummary(
            kind="dependency_blocked",
            text=f"Blocked because dependency {X.id} failed.",
            source_task_id=X.id,
        )
        D.status = FAILED
    persist
    notify_child_terminal_changed(X.task_center_harness_graph_id)
```

Dependency-blocked descendants are terminalized immediately. The evaluator sees:

- DONE sibling summaries
- FAILED sibling summaries

The evaluator then chooses one terminal:

- `submit_task_success` if the partial result is acceptable
- `launch_plan_handoff` if a new planner can fill the gap
- `submit_evaluation_failure` if the parent goal cannot be met

### Evaluator Hard Fail

```
evaluator E submits submit_evaluation_failure(summary)
        |
        v
TaskCenter.submit_evaluation_failure(E):
    E.summaries += TaskSummary(kind="evaluation_failure", text=summary, source_task_id=E.id)
    E.status = FAILED
    close_harness_graph_failed(E.task_center_harness_graph_id)
```

`close_harness_graph_failed(graph_id)`:

1. Mark the graph planner FAILED.
2. Append `child_failure` to `graph.root_task_id`.
3. Mark `graph.root_task_id` FAILED.
4. If the parent task belongs to another harness graph, notify child terminal state change for that outer graph.
5. If the parent task is the root executor, terminate the run FAILED.

### Evaluator Success

```
evaluator E submits submit_task_success(summary)
        |
        v
TaskCenter.submit_task_success(E):
    E.summaries += TaskSummary(kind="success", text=summary, source_task_id=E.id)
    E.status = DONE
    close_harness_graph_success(E.task_center_harness_graph_id)
```

`close_harness_graph_success(graph_id)`:

1. Mark the graph planner DONE.
2. Append `child_success` to `graph.root_task_id`.
3. Mark `graph.root_task_id` DONE.
4. If the parent task belongs to another harness graph, notify child terminal state change for that outer graph.
5. If the parent task is the root executor, terminate the run DONE.

## Critical Files And Changes

### 1. Rename `submit_task_completion` to `submit_task_success`

Audit and update:

- `backend/src/tools/mode_tool/submit_task_completion.py` -> `submit_task_success.py`
- `tools/mode_tool/__init__.py` registration
- `TaskCenter.submit_task_completion()` -> `TaskCenter.submit_task_success()`
- built-in agent terminal lists and prompts
- tests and schema summaries
- persistence/audit/event literal strings

### 2. Update `Task`

- Rename `spec` to `input`.
- Remove `parent_id`.
- Remove `closes_for`.
- Remove scalar `summary`.
- Add `task_center_harness_graph_id`.
- Add `summaries: list[TaskSummary]`.
- Do not add `pending_reason`; dependency-blocked descendants become FAILED.
- Widen `TaskRole` to `Literal["executor", "planner", "evaluator"]`.

### 3. Rename and reshape persisted graph records

- Rename `task_center_graph` to `task_center_harness_graph`.
- Store one row per harness graph, not one graph row per task.
- Add `root_task_id`, `planner_task_id`, `evaluator_task_id`, and `executor_task_ids`.
- Move task-level topology out of persisted task rows except `task_center_harness_graph_id` and `needs`.

### 4. Add graph helpers

- `parent_goal(task_id)`
- `planner_handoff(task_id)`
- `completed_dependencies(task_id)`
- `failed_dependencies(task_id)`
- `dependency_blocked_descendants(task_id)`
- `is_harness_graph_ready_for_evaluation(graph_id)`

`ready_tasks()` should continue to use only `needs`; it should not consult parent task state.

### 5. Replace closure propagation with harness graph closure

Delete `propagation.close_with_summary` and avoid a failed sibling walker. TaskCenter should own:

- `close_harness_graph_success(graph_id, summary_source_task_id)`
- `close_harness_graph_failed(graph_id, summary_source_task_id)`
- `notify_child_terminal_changed(graph_id | None)`

This keeps success/failure propagation tied to the decomposition boundary instead of hidden task pointers.

### 6. New and updated mode tools

- `submit_task_success(summary)` appends a summary entry and marks executor/evaluator success according to role.
- `submit_task_failure(summary)` rejects non-executors.
- `submit_evaluation_failure(summary)` rejects non-evaluators.
- `launch_plan_handoff(task_detail)` appends a handoff summary, builds planner launch context from the caller role, and creates a harness graph.
- `submit_plan_handoff(tasks, task_inputs, handoff_summary)` appends the planner handoff summary and creates child executors plus evaluator.

### 7. Prompt-context plumbing

- Planner prompt: `PlannerLaunchContext`. Nested/recovery prompts must include caller input, parent goal, prior planner handoff, DONE child summaries, FAILED child summaries, dependency-blocked summaries, and caller recovery/decomposition task detail when that evidence exists.
- Executor prompt: own input plus completed direct dependency summaries only.
- Evaluator prompt: parent task input, planner handoff summaries, DONE child summaries, and FAILED child summaries, including dependency-blocked descendants.

## Verification

End-to-end tests in `backend/tests/test_task_center/`:

1. Simple task success: root executor -> `submit_task_success`; no harness graph; root DONE.
2. Simple task failure: root executor -> `submit_task_failure`; root FAILED.
3. Plan-driven happy path: executor -> `launch_plan_handoff` -> planner -> `submit_plan_handoff` -> children DONE -> evaluator -> `submit_task_success`; harness graph closes parent task DONE.
4. Soft fail: one child fails; dependency-blocked descendants become FAILED; evaluator schedules once no executor child remains unfinished.
5. Hard fail: evaluator -> `submit_evaluation_failure`; harness graph planner and parent task become FAILED; outer graph child terminal state change is notified.
6. Nested graph recovery: inner evaluator hard-fails; the outer graph sees its child task FAILED and can schedule the outer evaluator.
7. Evaluator-driven replan: evaluator -> `launch_plan_handoff`; new harness graph has `root_task_id` equal to the evaluator task, and the spawned planner input includes parent goal, prior planner handoff, DONE child summaries, FAILED child summaries, dependency-blocked summaries, and evaluator recovery task detail.
8. Graph helpers: planner/evaluator see correct parent input and handoff summaries; executor children see only dependency summaries.
9. Evaluator dispatch under partial failure: DONE children plus FAILED children schedules evaluator when no executor child remains unfinished.
10. Role rejection: executor cannot call `submit_evaluation_failure`; evaluator cannot call `submit_task_failure`.
11. Summary history: handoff summary, executor success summary, evaluator success summary, and propagated child summary all coexist without overwriting.

Existing tests to update:

- `tests/test_agents/test_modes.py`
- `tests/test_engine/test_mode_gate.py`
- `tests/test_tools/test_schema_summary.py`
- `tests/test_task_center/test_submission_tools.py`
- `tests/test_task_center/test_persistence.py`
- `tests/test_task_center/test_task.py`
- `tests/test_task_center/test_graph.py`
- `tests/test_task_center/test_center.py`
- `tests/test_task_center/test_task_center_graph_snapshots.py`
- `tests/test_task_center/test_task_prompt_context.py`

## Implementation Order

1. Rename `submit_task_completion` to `submit_task_success`.
2. Add `TaskSummary`; change scalar `summary` to `summaries`.
3. Rename `Task.spec` to `Task.input`.
4. Remove `Task.parent_id`, `Task.closes_for`, and any `pending_reason` plan.
5. Rename and reshape `task_center_graph` into `task_center_harness_graph`.
6. Add harness graph creation in `launch_plan_handoff`.
7. Add graph helpers for parent input, handoff summaries, dependency summaries, dependency-blocked descendants, and evaluator readiness.
8. Replace closure propagation with harness graph closure methods.
9. Add executor/evaluator failure tools and role guards.
10. Update built-in planner, executor, and evaluator prompts.
11. Update prompt-context plumbing.
12. Update architecture docs and tests.

## Known Follow-ups

- Per-child completion contracts: each generator boundary may eventually need a more precise completion contract than planner-level task inputs.
- Replan depth cap: nothing bounds `executor -> planner -> child -> planner -> ...` chains. Track depth on harness graph lineage and reject `launch_plan_handoff` past a configured limit.
- Handoff heuristics: instrument escalations per task lineage to spot premature delegation patterns.

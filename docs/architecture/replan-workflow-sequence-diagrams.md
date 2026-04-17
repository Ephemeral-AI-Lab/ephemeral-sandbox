# Replan Workflow Sequence Diagrams

This document shows the task replanning lifecycle for the main runtime scenarios.
The current `submit_replan` payload is:

- `new_tasks`
- `cancel_ids`
- `output`

`team_replanner` is a normal expandable task. When original task `A` fails, `A`
moves to `REPLANNING`, replanner task `R` is created, and pending task graph
nodes that depended on `A` are rewired to depend on `R`. Any dependent of `A`
with a non-pending status is a graph invariant violation.

The scheduler invariant is strict: a task can be `READY` or `RUNNING` only when
all dependencies are `DONE`.

## 1. Failure Creates A Replanner

```mermaid
sequenceDiagram
    participant W as Worker Task A
    participant Ex as Executor
    participant Run as TeamRun
    participant TC as TaskCenter
    participant TS as TaskStore
    participant G as Task Graph
    participant R as Replanner Task R

    W->>Ex: submit_task_summary(type="fail", reason)
    Ex->>TC: request_replan(A, reason)
    TC->>TS: request_replan(A, reason, team_replanner)

    TS->>TS: mark A as REPLANNING
    TS->>TS: create R assigned to team_replanner
    TS->>TS: set R.fired_by_task_id = A
    TS->>TS: verify all dependents of A are PENDING

    alt every dependent of A is PENDING
        TS->>TS: replace A with R in pending dependents
        TS->>TS: recompute pending_dep_count
        TS->>G: refresh graph
        TS-->>TC: R
        TC->>G: emit task_added(R)
        TC->>G: emit status changes
    else any dependent of A is not PENDING
        TS-->>TC: GraphInvariantViolation
        TC-->>Ex: replan request fails
        Ex->>Run: fail_fast(graph_invariant_violation)
    end
```

The executor routes failure through `TaskCenter.request_replan` because the
executor only interprets the agent's terminal submission. TaskCenter owns the
task lifecycle boundary: replan budget checks, replanner selection, event
emission, and the atomic TaskStore mutation that creates `R` and rewires
pending dependents. A graph invariant violation is fatal; the executor fails
the team run immediately instead of treating it as a retryable worker error.

## 2. Replanner Submits No Direct Children

```mermaid
sequenceDiagram
    participant R as Replanner Task R
    participant Tool as submit_replan
    participant Ex as Executor
    participant TC as TaskCenter
    participant PE as PlanExpander
    participant TS as TaskStore
    participant D as Downstream Tasks
    participant A as Original Task A

    R->>Tool: submit_replan(new_tasks=[], cancel_ids=[...], output)
    Tool->>Tool: validate cancel_ids and new_tasks
    Tool-->>Ex: AgentResult(submitted_replan)

    Ex->>TC: complete_task(R, submitted_replan)
    TC->>PE: apply_replan(R, new_tasks, cancel_ids)
    PE->>TS: apply_replan_atomic(cancel_ids, specs=[])

    TS->>TS: cancel requested non-terminal tasks with cascade
    TS-->>PE: inserted=[]
    PE-->>TC: replanner_child_count=0

    TC->>TS: mark R DONE
    TS->>D: promote dependents whose pending deps reach 0
    TC->>TS: finalize_replanned_origin(R)
    TS->>A: mark A FAILED without cascade
```

## 3. Replanner Creates Direct Children

```mermaid
sequenceDiagram
    participant R as Replanner Task R
    participant TC as TaskCenter
    participant PE as PlanExpander
    participant TS as TaskStore
    participant C as Children Of R
    participant D as Downstream Tasks
    participant A as Original Task A

    R->>TC: complete_task(R, submitted_replan with child tasks)
    TC->>PE: apply_replan(R, new_tasks)
    PE->>TS: apply_replan_atomic(specs include parent_id=R)

    TS->>TS: insert child tasks under R
    TS-->>PE: inserted children
    PE-->>TC: replanner_child_count > 0

    TC->>TS: mark R EXPANDED
    Note over D,A: D still waits on R. A stays REPLANNING.

    C->>TC: child completes DONE
    TC->>TS: mark child DONE
    TS->>TS: maybe_promote_expanded_parent(child)

    alt all direct children of R are DONE
        TS->>R: mark R DONE
        TS->>D: promote downstream dependents
        TC->>TS: finalize_replanned_origin(R)
        TS->>A: mark A FAILED without cascade
    else any child is FAILED or CANCELLED
        TS->>R: keep R EXPANDED
        Note over D,A: downstream remains blocked on R
    end
```

## 4. Replanner Adds Sibling Or Subtree Tasks Only

```mermaid
sequenceDiagram
    participant R as Replanner Task R
    participant TC as TaskCenter
    participant PE as PlanExpander
    participant TS as TaskStore
    participant S as Sibling Layer Or Subtree
    participant D as Downstream Tasks
    participant A as Original Task A

    R->>TC: complete_task(R, submitted_replan)
    TC->>PE: apply_replan(R, new_tasks)

    PE->>PE: validate parent_id is in allowed parent projection
    PE->>PE: count direct children where parent_id == R
    PE->>TS: apply_replan_atomic(specs under R.parent or sibling subtree)

    TS->>S: insert sibling or subtree tasks
    TS-->>PE: inserted tasks
    PE-->>TC: replanner_child_count=0

    TC->>TS: mark R DONE
    TS->>D: downstream unlocks if all deps are DONE
    TC->>TS: finalize_replanned_origin(R)
    TS->>A: mark A FAILED without cascade
```

Sibling-layer or sibling-subtree additions do not make `R` `EXPANDED`. Only
direct children of `R` do.

## 5. Replanner Cancels A Running Task

```mermaid
sequenceDiagram
    participant R as Replanner Task R
    participant PE as PlanExpander
    participant Run as Active Agent Run
    participant TS as TaskStore
    participant X as Running Task X

    R->>PE: submit_replan(cancel_ids=[X])
    PE->>PE: validate X is non-terminal and in allowed projection
    PE->>PE: compute cascaded descendants and dependency dependents

    alt X or a cascaded task is RUNNING
        PE->>Run: cancel_agent_run(task_id)
        Run-->>PE: cancellation requested
    end

    PE->>TS: apply_replan_atomic(cancel_ids=[X])
    TS->>X: mark CANCELLED
    TS->>TS: cascade cancel active descendants and dependents
```

Active runner cancellation is requested before the task is marked cancelled in
storage.

## 6. Invalid Replan Submission

```mermaid
sequenceDiagram
    participant R as Replanner Task R
    participant Tool as submit_replan
    participant PE as PlanExpander
    participant TC as TaskCenter
    participant A as Original Task A

    R->>Tool: submit_replan(...)
    Tool->>Tool: schema and graph validation

    alt invalid at tool layer
        Tool-->>R: validation error
        Note over R: R keeps running and can resubmit.
    else invalid at runtime layer
        Tool-->>TC: submitted_replan
        TC->>PE: apply_replan(...)
        PE-->>TC: InvalidPlan or BudgetExceeded
        TC->>A: fail original REPLANNING task with replan_apply_failed
        TC-->>R: error propagated
    end
```

Tool-layer validation is recoverable inside the replanner turn. Runtime apply
failure fails the original replanning task so it cannot remain stuck.

## 7. Replanner Fails

```mermaid
sequenceDiagram
    participant R as Replanner Task R
    participant Ex as Executor
    participant TC as TaskCenter
    participant TS as TaskStore
    participant A as Original Task A
    participant D as Downstream Tasks

    R->>Ex: runner_exception or fail
    Ex->>TC: fail_task(R, reason)

    TC->>TS: check R.fired_by_task_id
    TS-->>TC: A

    alt A is still REPLANNING
        TC->>TS: fail_with_cascade(A, replanner_failed)
        TS->>A: mark FAILED
        TS->>D: cascade cancel active dependents still affected
    end

    TC->>TS: fail_task(R, reason)
    TS->>R: mark FAILED
```

A successful replanner finalizes `A` without cascade. A failed replanner fails
the recovery path.

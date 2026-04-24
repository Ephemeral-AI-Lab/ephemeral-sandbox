# Team Failure Conditions

This document separates whole-run failure from task-local failure in team
coordination. A worker task can fail without failing the team run if replanning
absorbs the failure. The team run fails only through `fail_fast(...)` or when
final status computation sees the root task in `failed`.

## Failure Categories

| Category | Condition | Run result | Notes |
| --- | --- | --- | --- |
| Fatal invariant | `GraphInvariantViolation` during ready dispatch, running transition, replan dependency rewiring, or failure cleanup | `failed` immediately | The executor calls `TeamRun.fail_fast("graph_invariant_violation: ...")` because the task graph is no longer schedulable with confidence. |
| Fatal budget | `BudgetExceeded` while expanding a submitted plan, creating a replanner, or applying a replan during execution | `failed` immediately | Task budget and replan budget are run-level guarantees, not task-local failures. |
| Root task terminal failure | Root task reaches `failed` | `failed` at `TeamRun.wait()` finalization | The final status reflects the root task outcome unless an earlier fatal failure reason exists. |
| Root task direct execution failure | Root agent is unknown, the root runner crashes, context construction raises, or root cleanup fails into task failure | Usually `failed` | These first mark the root task failed or request replanning. The run fails if recovery does not produce a successful root outcome. |
| Invalid root plan | Root planner submits no plan or an invalid plan | `failed` | `PlanExpander` marks the planner task failed with `InvalidPlan: ...`; because the task is the root, final run status is failed. |
| Failed recovery path | Replanner task fails or crashes | `failed` through fail-fast | The original task stays terminal at `request_replan`; the replanner task failure is a normal `FAILED` outcome and aborts the run. |
| Invalid runtime replan | Runtime `apply_replan(...)` rejects a submitted replan | `failed` if this failure reaches the root | The original task stays terminal at `request_replan`; the replanner error follows normal task failure handling. |
| Detached-child roll-up | Every child of an expanded parent is detached, with no successful child | parent roll-up synthesis | Detached children do not synthesize parent failure. Expanded parents promote once every live child is terminal; the coordinator synthesizes the authoritative roll-up from available child submissions. |

## Task-Local Failure

The following conditions fail or replan a task but do not by themselves fail the
team run:

- A worker calls `request_replan(reason=...)`; the executor
  converts this into `TaskStatusUpdate(REQUEST_REPLAN, ...)` for
  `TaskCoordinator`.
- An agent exits without calling a terminal submission tool; the runner writes a
  failure summary and the executor treats it as a replan request.
- A non-root planner submits an invalid plan; the planner task fails and parent
  promotion decides whether the failure is absorbed or propagates.
- A non-root worker runner raises a normal exception; the task fails or enters
  replanning through normal executor cleanup.

These failures become run failures when recovery produces a `FAILED` outcome or
the root task ultimately becomes `failed`.

## Non-Fatal Conditions

Several errors are intentionally not run-fatal:

- `Executor.post_dispatch(...)` hook failures are logged after the status update
  has already been handled.
- Event-store append failures are logged and ignored so coordination can
  continue.
- Completion note post failures are logged and ignored.
- Scope warnings are injected when possible; injection failures do not fail the
  task.

## Non-Run Errors

Some validation errors happen before a team run becomes active and should not be
counted as failed team runs:

- Starting a root task without a non-empty `spec`.
- Starting from a team definition whose `entry_planner` is not registered.
- Starting with budgets too small to create the root task.
- Rehydrating an event log that is missing or malformed.

These raise to the caller instead of producing a normal `TeamRunStatus.FAILED`
event.

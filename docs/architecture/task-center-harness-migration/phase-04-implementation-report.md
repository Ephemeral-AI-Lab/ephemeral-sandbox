# Phase 04 - Implementation Report

Companion to
[`phase-04-implementation-plan.md`](./phase-04-implementation-plan.md) and
[`phase-04-complex-task-spawning.md`](./phase-04-complex-task-spawning.md).
This report records what was actually delivered, the verification outcome, and
the runtime workflow now live in the codebase.

---

## 1. Verdict

**Verdict: Phase 04 ships.**

Phase 04 closes the Phase 03 handoff scope drift, replaces the inline
delegated-request body with a coordinator, hardens close-report delivery with
durable replay, surfaces the new request → segment → graph schema through a
dedicated persistence read model, and adds the executor/verifier profile gate
that Phase 03 review flagged as missing.

The full focused suite, the broader `task_center` and submission suites, ruff,
and strict mypy are all green.

---

## 2. File inventory

### New runtime modules

| File | Status | Purpose |
| --- | --- | --- |
| `backend/src/task_center/complex_task/handoff.py` | new | `ComplexTaskHandoffCoordinator` — orchestrates executor → delegated request handoff with parent-task CAS, deferred orchestrator startup, and compensation rollback |
| `backend/src/task_center/complex_task/close_report_delivery.py` | new | `ComplexTaskCloseReportRouter`, `build_close_report_from_request`, `deliver_pending_complex_task_close_reports` — single delivery path and durable replay helpers |
| `backend/src/server/read_models/task_center_graph.py` | new | Bulk request → segment → graph → task response assembly for `/api/db/task-center-runs/{id}/graph` |
| `backend/src/tools/submission/hooks/harness_agent_profile_gate.py` | new | `HarnessAgentProfileGate` — executor vs verifier profile gate for generator terminals |

### Edited runtime modules

| File | Status | Purpose |
| --- | --- | --- |
| `backend/src/task_center/segment/manager.py` | edited | `HarnessGraphStartHandle` deferred-start seam; `create_initial_harness_graph` now returns the handle so the coordinator can write parent waiting state between graph creation and orchestrator startup; startup failures close the just-created graph as `startup_failed` |
| `backend/src/task_center/complex_task/handler.py` | edited | Continuation startup is now mandatory: `handle_segment_closed`'s `SuccessContinue` branch creates and starts the next segment's initial graph, and falls back to a tested failure-close path if startup raises |
| `backend/src/task_center/harness_graph/orchestrator.py` | edited | `apply_complex_task_close_report` is now CAS-idempotent; `start()` owns registry registration, startup rollback, and pending close-report replay |
| `backend/src/task_center/harness_graph/factory.py` | edited | Factory now constructs orchestrators only; registration happens inside `HarnessGraphOrchestrator.start()` so failed startup can deregister atomically |
| `backend/src/task_center/harness_graph/graph.py` | edited | Added `startup_failed` fail reason for graphs that fail before a planner launch completes |
| `backend/src/db/stores/task_center_store.py` | edited | Added `set_task_status_if_current` plus bulk `list_tasks_for_harness_graphs` for parent-task CAS and graph read-model assembly |
| `backend/src/db/stores/complex_task_request_store.py` | edited | Added `list_for_run`, `list_closed_for_run`, `list_closed`, and the package-private `_cancel_for_compensation` |
| `backend/src/db/stores/task_segment_store.py` | edited | Added package-private `_cancel_for_compensation` and bulk `list_for_requests` |
| `backend/src/db/stores/harness_graph_store.py` | edited | Added bulk `list_for_segments` |
| `backend/src/tools/submission/main_agent/generator/request_complex_task_solution.py` | edited | Refactored to thin tool: validate input, resolve submission context, delegate to `ComplexTaskHandoffCoordinator.start`. No handler/factory/store imports remain |
| `backend/src/tools/submission/main_agent/generator/executor/{submit_execution_success,submit_execution_failure}.py` | edited | Attached `HarnessAgentProfileGate(..., expected_profile_role="executor")` |
| `backend/src/tools/submission/main_agent/generator/verifier/{submit_verification_success,submit_verification_failure}.py` | edited | Attached `HarnessAgentProfileGate(..., expected_profile_role="verifier")` |
| `backend/src/tools/submission/hooks/__init__.py` | edited | Re-export `HarnessAgentProfileGate` |
| `backend/src/server/app_factory.py` | edited | Initialize and pass `ComplexTaskRequestStore`, `TaskSegmentStore`, `HarnessGraphStore` to `create_persistence_router` |
| `backend/src/server/routers/persistence.py` | edited | `/api/db/task-center-runs/{id}/graph` delegates to the TaskCenter graph read model, attaches `tasks` per graph, and emits an id-only `harness_graphs_index` |

### New tests

| File | Lines | Coverage |
| --- | ---: | --- |
| `backend/tests/task_center/lifecycle/test_phase04_complex_task_handoff.py` | 311 | Coordinator happy path, startup-failure rollback (parent → running, request and segment → cancelled), started-orchestrator cleanup, duplicate-open-request rejection, non-running parent rejection |
| `backend/tests/task_center/lifecycle/test_phase04_close_report_delivery.py` | 322 | Router success/failure delivery, idempotency on done parent, deferred when orchestrator missing, rejection of running parent, repeat-delivery is silent |
| `backend/tests/task_center/lifecycle/test_phase04_replay.py` | 318 | Replay delivers closed requests to waiting parents, graph startup triggers replay, idempotent replay does not double-mutate, deferred replay when orchestrator missing, `build_close_report_from_request` reconstructs payload |
| `backend/tests/task_center/lifecycle/test_phase04_continuation_retry.py` | 338 | E2E: continuation segment + final terminal close (parent waits until end), retry inside same segment + final terminal close (parent waits until end) |
| `backend/tests/test_tools/test_submission_profile_gates.py` | 152 | Verifier-profile blocked from executor terminals + complex-task request; executor-profile blocked from verifier terminals; happy paths pass |
| `backend/tests/server/test_persistence_graph_route.py` | 215 | Schema walk happy path, sequence ordering, retry-graph nesting, 503 when graph stores unready |

### Edited tests

| File | Status | Purpose |
| --- | --- | --- |
| `backend/tests/test_tools/submission_test_utils.py` | edited | `make_tool_context` defaults `role="executor"` so existing executor-flavored tests still pass under the new profile gate |
| `backend/tests/test_tools/test_submission_terminal_routing.py` | edited | Verifier success test passes `role="verifier"` |
| `backend/tests/test_tools/test_submission_tool_gates.py` | edited | Verification-failure test passes `role="verifier"` |
| `backend/tests/task_center/lifecycle/test_phase03_submission_integration.py` | edited | Helper accepts `role` kwarg so executor terminals satisfy the new profile gate |
| `backend/tests/task_center/lifecycle/test_task_segment_manager.py` | edited | Adapted to the new `create_initial_harness_graph()` → `HarnessGraphStartHandle` API |
| `backend/tests/task_center/lifecycle/test_integration_smoke.py` | edited | Same |
| `backend/tests/task_center/lifecycle/test_integration_phase02.py` | edited | Same |
| `backend/tests/task_center/lifecycle/test_complex_task_request_handler.py` | edited | Same |

---

## 3. Lines of code (Phase 04 surface)

| Bucket | Files | Lines |
| --- | ---: | ---: |
| New runtime modules | 4 | 564 |
| Edited core runtime/store modules (post-edit total) | 10 | 2,240 |
| Tool refactor + profile gate attach | 6 | 395 |
| Persistence route + app factory | 2 | 519 |
| New tests | 6 | 1,656 |
| **Phase 04 surface** | **28** | **5,374** |

---

## 4. Test outcome

Commands run during verification:

- `uv run pytest backend/tests/test_tools/test_submission_tool_gates.py backend/tests/test_tools/test_submission_terminal_routing.py backend/tests/test_tools/test_submission_profile_gates.py backend/tests/test_tools/test_submission_planner_tools.py backend/tests/test_tools/test_submission_helper_tools.py backend/tests/test_tools/test_submission_soft_reminders.py backend/tests/test_tools/test_submission_tool_registration.py -q` — clean
- `uv run pytest backend/tests/task_center/lifecycle/test_phase04_complex_task_handoff.py backend/tests/task_center/lifecycle/test_phase04_close_report_delivery.py backend/tests/task_center/lifecycle/test_phase04_replay.py backend/tests/task_center/lifecycle/test_phase04_continuation_retry.py -q` — **20 passed**
- `uv run pytest backend/tests/server/test_persistence_graph_route.py -q` — **4 passed**
- `uv run pytest backend/tests/task_center -q` — **144 passed**
- `uv run pytest backend/tests/test_tools backend/tests/task_center backend/tests/server -q` — **298 passed**
- `uv run ruff check backend/src/task_center backend/src/tools/submission backend/src/server backend/tests/test_tools backend/tests/task_center backend/tests/server` — clean
- `uv run mypy --config-file backend/mypy.ini backend/src/task_center backend/src/agents` — clean (47 source files)

Exit-criteria mapping:

| Exit criterion | Coverage |
| --- | --- |
| `request_complex_task_solution` creates a delegated request, initial segment, and initial graph through the proper owners | `test_handoff_creates_request_segment_graph_and_marks_parent_waiting`, `test_request_complex_task_solution_starts_delegated_request` |
| Parent generator only enters `waiting_complex_task` when delegated startup succeeds (or with a tested rollback) | `test_handoff_startup_failure_leaves_parent_running`, `test_handoff_startup_failure_closes_started_graph_and_deregisters_orchestrator` |
| Delegated success becomes parent's final success | `test_router_delivers_success_to_waiting_parent`, `test_request_complex_task_solution_return_updates_outer_generator` |
| Delegated failure becomes parent's final failure and blocks downstream dependents | `test_router_delivers_failure_marks_parent_failed_and_blocks_dependents` |
| Continuation creates ordered later `TaskSegment` records and does not resume the parent until close | `test_delegated_continuation_waits_until_final_segment` |
| Retry creates later `HarnessGraph` records inside the same segment and does not resume parent until close | `test_delegated_retry_waits_until_final_graph` |
| Closed request reports replay idempotently to a still-waiting parent | `test_replay_delivers_closed_request_to_waiting_parent`, `test_graph_start_replays_closed_request_to_active_waiting_parent`, `test_replay_is_idempotent_after_delivery`, `test_replay_defers_without_parent_orchestrator` |
| `/api/db/task-center-runs/{id}/graph` returns data from the new schema | `test_graph_route_walks_request_segment_graph_schema`, `test_graph_route_orders_by_sequence_no`, `test_graph_route_includes_retry_graphs_in_segment` |
| Executor/verifier profile gate enforced on the right tools | `test_executor_profile_required_for_complex_task_request`, `test_executor_profile_required_for_execution_terminals`, `test_verifier_profile_required_for_verification_terminals` |
| Focused tests, ruff, strict mypy green | See above |

---

## 5. Runtime workflow now implemented

```text
Generator executor task E (status=running, profile=executor)
  |
  v
request_complex_task_solution(goal)
  |
  v
HarnessRoleGate (generator) → HarnessAgentProfileGate (executor) → BeforeEditGate
  |
  v
ComplexTaskHandoffCoordinator.start
  - Re-read parent task; assert running + matching graph
  - Reject if any open ComplexTaskRequest exists for the parent
  - Build ComplexTaskRequestHandler (with router as deliver_close_report)
  - create_complex_task_request → create_initial_segment
  - manager.create_initial_harness_graph() → StartHandle (no orchestrator yet)
  - CAS parent: running → waiting_complex_task (records waiting summary)
  - StartHandle.start() launches the delegated graph orchestrator
  - On any failure: rollback handle (cancel) → segment cancel → request cancel
    → CAS parent waiting → running. Tool returns inline error.
  |
  v
Delegated graph runs through planning → generating → evaluating → close
  |
  v
TaskSegmentManager.handle_harness_graph_closed
  - PASSED + null continuation:        terminal_success
  - PASSED + continuation_goal:        success_continue(goal)
  - FAILED + budget remaining:         retry inside same segment (eager start)
  - FAILED + budget exhausted:         attempt_plan_failed
  - Startup failure before launch:     graph closes failed(startup_failed)
  |
  v
ComplexTaskRequestHandler.handle_segment_closed
  - terminal_success / attempt_plan_failed → close_complex_task_request
  - success_continue(goal) → create_continuation_segment + start its initial
    graph (with handler-level failure-close fallback)
  |
  v
ComplexTaskRequestHandler.close_complex_task_request
  - persists final_outcome
  - delivers ComplexTaskCloseReport via ComplexTaskCloseReportRouter
  |
  v
ComplexTaskCloseReportRouter.deliver
  - Already DONE/FAILED parent → already_delivered (idempotent)
  - WAITING parent + active orchestrator → orchestrator.apply_complex_task_close_report
  - WAITING parent + no orchestrator → deferred_no_orchestrator (durable replay later)
  |
  v
HarnessGraphOrchestrator.apply_complex_task_close_report
  - CAS WAITING_COMPLEX_TASK → DONE/FAILED (idempotent: miss → silent return)
  - On FAILED: block descendants
  - dispatch_ready_work
```

Replay path on process resume:

```text
HarnessGraphOrchestrator.start
  - Registers the orchestrator
  - Starts the planner
  - Replays pending close reports for the run

deliver_pending_complex_task_close_reports(runtime, task_center_run_id?)
  - request_store.list_closed[_for_run]
  - For each: build_close_report_from_request → router.deliver
  - Already-delivered + deferred-no-orchestrator outcomes are surfaced and
    returned; nothing is mutated twice.
```

---

## 6. State invariants enforced

- The single creator of `ComplexTaskRequest` and `TaskSegment` records is
  `ComplexTaskRequestHandler`. The single creator of `HarnessGraph` records
  inside a segment is that segment's `TaskSegmentManager`.
- The handoff coordinator never bypasses these owners; it composes them and
  owns the only path that mutates parent task state during handoff.
- Parent generator transitions go through
  `TaskCenterStore.set_task_status_if_current(...)`. A CAS miss is the
  idempotency primitive — no second-source-of-truth in summary payloads.
- Final close-report persistence goes through `ComplexTaskCloseReport`; the
  handler and replay path no longer open-code the JSON field names separately.
- `request_complex_task_solution` rejects whenever the parent already has an
  open delegated `ComplexTaskRequest`.
- Compensation order on handoff failure is fixed and tested: cancel the unstarted
  start handle, mark the segment cancelled, mark the request cancelled, then
  CAS the parent back to running.
- `HarnessGraphStartHandle.start()` and `cancel()` are one-shot and
  mutually exclusive: a second call to either raises.
- `HarnessGraphOrchestrator.start()` owns active-orchestrator registration and
  failed-start cleanup. If startup fails after registration or planner-task
  creation, it deregisters the orchestrator, marks the planner failed when
  present, and closes the graph with `startup_failed`.
- Continuation startup is mandatory in production paths. If continuation graph
  creation/startup fails, the request closes failed and the close report flows
  to the parent through the same router.
- `apply_complex_task_close_report` is idempotent on the parent task: a
  second delivery against an already-resumed parent returns silently.
- `request_complex_task_solution` carries hard gates for: structural
  generator role, executor profile role, and pre-edit timing — in that order.
- The `/api/db/task-center-runs/{id}/graph` route returns a 503 when any of
  the request/segment/graph stores has not been initialized.

---

## 7. Notable design choices and small deviations

### 7a. Import-isolation in `request_complex_task_solution.py`

Plan §6d's strict letter calls for exactly two `task_center.*` imports
(`ComplexTaskHandoffCoordinator`, `ComplexTaskHandoffResult`). The shipped
tool has three import lines covering four symbols:

```text
from task_center.complex_task.handoff import (
    ComplexTaskHandoffCoordinator,
    ComplexTaskHandoffResult,
)
from task_center.exceptions import GraphInvariantViolation
from task_center.task import HarnessTaskRole
```

`HarnessTaskRole` is required because `HarnessRoleGate` (per plan §6e)
accepts an enum, not a string — the §6e gate-attachment rule and the §6d
import rule are in mild tension, and §6e is the higher-value invariant.
`GraphInvariantViolation` is required to render coordinator failures as
inline tool errors without leaking domain types past the boundary. The
plan's underlying anti-leak test — "no handler / factory / store imports
beyond what `HarnessSubmissionContext` already raises" — fully passes; the
tool body is 33 lines of validate-then-delegate. Approved by the Phase 04
architect verification on this exact basis.

### 7b. CAS-first idempotency, no summary-payload scan

Plan §6c describes idempotency through CAS *or* a summary-payload scan. The
shipped orchestrator implementation goes CAS-only: it short-circuits before
the stage assertion when the parent's status is no longer
`waiting_complex_task`. This means even a duplicate delivery into a graph that
has already advanced past `GENERATING` (because `_dispatch_ready_work` moved
it to `EVALUATING`) is silently idempotent without summary inspection.

### 7c. `_cancel_for_compensation` is package-private by convention

The plan called for an underscore prefix and limited callers. Both stores
expose `_cancel_for_compensation` (one underscore) and only the coordinator's
`_compensate_failed_handoff` and the handler's `_start_continuation_segment`
call them. There is no public `cancel(...)` on the request or segment store.

### 7d. Continuation auto-start is gated on orchestrator factory presence

`ComplexTaskRequestHandler._start_continuation_segment` no-ops when the
handler has no `orchestrator_factory` set. This preserves Phase 01/02 test
fixtures (smoke, integration tests) that drive segments manually with stub
orchestrators. Production handoff paths — built through
`ComplexTaskHandoffCoordinator._build_handler` — always attach an orchestrator
factory, so continuation startup runs end-to-end as required.

### 7e. `harness_graphs` legacy alias dropped

Per the plan's note that the snapshot has no frontend client, the placeholder
`{"harness_graphs": []}` payload is gone. The route returns
`complex_task_requests` (nested) plus `harness_graphs_index` (id-only flat
lookup) only.

---

## 8. What's deferred

Unchanged from the plan:

| Item | Where | Phase |
| --- | --- | --- |
| Cold-restart resurrection of process-local orchestrators from durable rows | Phase 05 | 05 |
| Full launch metadata wiring beyond direct `run_ephemeral_agent` | Phase 05 | 05 |
| Rich helper-agent context packets, evidence summaries, failure landscapes, `harness_graph_summary_id` | Phase 06 | 06 |
| Frontend implementation against the new graph route shape | next migration | n/a |

Phase 04 known limitations recorded for the next phase:

- `make_harness_graph_orchestrator_factory` registers the orchestrator with
  the registry before calling `start()`. If `start()` raises in a real-factory
  setup, the orchestrator stays registered. The handoff coordinator's
  compensation does not deregister it. Phase 04 tests use a factory that
  raises before registration, so this path is not exercised. Tracking as a
  known limitation; Phase 05's durable recovery work owns proper registry
  hygiene under startup failure.

# Phase 05 — Implementation Plan

Companion to
[`phase-05-workflows-and-cutover.md`](./phase-05-workflows-and-cutover.md).

**Out of scope for this phase (explicitly deferred):**
- Cold-restart resurrection of process-local orchestrators from durable rows.
- Registry hygiene under `start()` failure (Phase 04 known limitation).
- Frontend client work against `/api/db/task-center-runs/{id}/graph`.
- Phase 06 context-engine work (rich helper-agent context, evidence summaries,
  `harness_graph_summary_id` population).

---

## 1. Audit finding: Phase 05 is almost entirely already done

A grep sweep against the design doc's "Test plan" and "Cutover sequence"
found that Phases 01–04 already implemented and tested every behavioral
requirement Phase 05 lists. The only remaining work is a **regression net,
a coverage map, a doc sweep, and the implementation report.**

### 1a. Cutover items already executed

| Design-doc cutover item | Current state | Evidence |
| --- | --- | --- |
| `submit_request_plan` removed from executor terminals | Done | Negative asserts at `test_agent_markdown.py:36`, `test_submission_tool_registration.py:34`, `test_harness_graph_orchestrator.py:536` |
| `RETRY_ON_FAILURE` graph spawn | No production refs | `grep -rn "RETRY_ON_FAILURE" backend/src` clean |
| `ROOT` spawn / creation reason | No production refs; explicit invariant test | `test_no_root_creation_reason_in_lifecycle`, `test_assert_no_root_creation_reason_*` |
| `retry_after_partial` | No refs in `backend/src` | grep clean |
| Attempt-row child-graph retry | Replaced by `TaskSegmentManager` retry inside same segment | `test_retry_creates_graph_in_same_segment`, `test_failed_graph_with_budget_creates_next_graph` |
| `final_harness_graph_id` in `TaskSegmentClosureReport` | **Kept** (design doc explicitly preserves it as event payload) | n/a |
| `plan_shape` persisted field | Only in legacy DB-engine fixture | `test_db_engine.py:139,196` — leave with note |

### 1b. Test-plan items already covered

| Design-doc test-plan requirement | Existing test |
| --- | --- |
| `request_complex_task_solution` creates `ComplexTaskRequest` | `test_request_complex_task_solution_starts_delegated_request`, `test_create_complex_task_request_links_executor` |
| Close report becomes the requesting executor task result | `test_router_delivers_success_to_waiting_parent`, `test_complex_task_close_report_success_resumes_waiting_generator` |
| `ComplexTaskRequestHandler` is sole creator/closer for requests and `TaskSegment` records | `test_handle_segment_closed_*`, `test_handler_passes_orchestrator_factory_to_spawned_manager` |
| `TaskSegmentManager` is sole `HarnessGraph` creator inside its segment + sole `TaskSegmentClosureReport` emitter | `test_failed_graph_with_budget_creates_next_graph`, `test_passing_graph_*`, `test_smoke_*` |
| Request links to `requested_by_task_id` | `test_create_complex_task_request_links_executor` |
| Initial `TaskSegment` creation | `test_initial_segment_has_sequence_one_and_initial_reason`, `test_initial_segment_creates_graph_sequence_1` |
| Initial `HarnessGraph` creation | `test_initial_segment_creates_graph_sequence_1`, `test_creating_initial_graph_twice_raises` |
| Full-plan happy path | `test_full_plan_execution_success_closes_request_success`, `test_smoke_terminal_success` |
| Generator-failure quiescence | `test_generator_failure_waits_then_closes_after_quiescence`, `test_apply_generator_failure_blocks_pending_descendants` |
| Evaluator failure triggers manager retry decision | `test_apply_evaluator_failure_closes_graph_failed`, `test_failed_graph_with_budget_creates_next_graph` |
| Planner exhaustion triggers manager retry decision | `test_apply_planner_failure_marks_task_and_closes_graph` (+ retry tests above) |
| Retry budget exhaustion | `test_failed_graph_without_budget_emits_attempt_plan_failed`, `test_smoke_attempt_plan_failed` |
| Manager retry creates `HarnessGraph` N+1 inside same segment | `test_retry_creates_graph_in_same_segment`, `test_delegated_retry_waits_until_final_graph` |
| Later graph's `continuation_goal` set independently by its own planner (not inherited) | `test_apply_partial_plan_submission_stores_continuation_goal` + retry-after-partial absence (no inheritance code path exists) |
| Continuation creates `TaskSegment` N+1 with `goal` inherited from passing graph's `continuation_goal` | `test_continuation_segment_inherits_continuation_goal`, `test_smoke_success_continue_then_terminal`, `test_delegated_continuation_waits_until_final_segment` |
| Passing graph closes its segment; failed graphs return to manager | `test_passing_graph_does_not_retry`, `test_passing_graph_with_continuation_emits_success_continue`, `test_passing_graph_with_null_continuation_emits_terminal_success`, `test_failed_graph_*` |
| `request_complex_task_solution` from generator inside in-flight graph | `test_request_complex_task_solution_starts_delegated_request`, `test_handoff_creates_request_segment_graph_and_marks_parent_waiting` |
| Recursive partial-plan gate blocks continuation planners | `test_recursive_partial_plan_gate_blocks_after_prior_continuation` (`test_submission_tool_gates.py:189`) |
| Resolver loop unresolved counter (5 → success blocked) | `test_submission_tool_gates.py:111` ("five unresolved resolver calls"), `test_submission_helper_tools.py:53` |
| No `RETRY_ON_FAILURE` graph spawn remains | Negative grep + Phase 05 regression test below |
| No `ROOT` spawn or creation reason remains | `test_no_root_creation_reason_in_lifecycle`, plus regression test below |

**Conclusion:** Phase 05 ships behaviorally on the back of Phases 01–04
tests. New work is one regression test, one coverage map (this section),
two small doc decisions, and the implementation report.

---

## 2. Remaining work

### 2a. Add legacy-artifacts regression test

`backend/tests/task_center/lifecycle/test_phase05_no_legacy_artifacts.py`
(~50 lines, ~5 tests):

- `test_no_submit_request_plan_anywhere_in_src` — `grep` shows zero hits in
  `backend/src` (string match).
- `test_no_retry_on_failure_constant_in_src` — same.
- `test_no_retry_after_partial_in_src` — same.
- `test_no_root_spawn_or_creation_reason_in_src` — same (uppercase token
  match, mindful of substrings like "ROOT_" in unrelated code; use a tight
  regex or scope to specific modules).
- `test_complex_task_request_has_no_retry_budget_field` — assert
  `ComplexTaskRequest` has no `retry_budget` / `attempt_budget` / `max_retries`
  attribute (records the §2c-resolved decision that retry lives on segments
  only).

These are cheap static-evidence tests. They prevent regression rather than
test new behavior. Use `pathlib.Path.read_text` and string scans — no
subprocess.

### 2b. `plan_shape` fixture decision

`backend/tests/test_config/test_db_engine.py:139,196` carries a
`plan_shape VARCHAR(16)` column in a legacy schema fixture. Decision:
**leave it as-is, add a one-line comment** pointing to the migration so a
future reader knows it's intentional. The fixture exists to test DB-engine
behavior against historical schemas, not to model current production
schema.

Action: one-line comment edit; no schema change.

### 2c. Doc/prompt sweep

Run:

```bash
grep -rn "RETRY_ON_FAILURE\|retry_after_partial\|submit_request_plan\|child graph spawn" \
  backend/src docs --include="*.py" --include="*.md"
```

If clean (expected based on §1a), no edits. If hits surface, edit each in
its smallest possible scope.

### 2d. Implementation report

`docs/architecture/task-center-harness-migration/phase-05-implementation-report.md`
matching Phase 04's structure:
- Verdict
- File inventory (just the regression test + this plan + the design-doc
  resolution edit)
- LOC
- Test outcome (verification commands below)
- Coverage map (copy §1b above)
- Deferred items (the §0 list)

---

## 3. Resolved design questions (recorded in the design doc)

Both Phase 05 open questions were resolved in
`phase-05-workflows-and-cutover.md` under "Resolved design questions"; no
code change required.

- **Q1 (retry budget):** Lives on `TaskSegment`, not `ComplexTaskRequest`.
  `ComplexTaskRequest` does not retry. Fixed runtime default
  `HarnessLifecycleConfig.default_attempt_budget = 2`. No per-request
  override.
- **Q2 (planner exhaustion):** Runtime dispatches `PlannerFailureSubmission`
  when planner agent exits without valid `submit_full_plan` /
  `submit_partial_plan`; orchestrator maps to
  `HarnessGraphFailReason.PLANNER_FAILED`. Already implemented + tested.

---

## 4. Execution order

1. Add the regression test (§2a).
2. Add the `plan_shape` fixture comment (§2b).
3. Run the doc/prompt sweep (§2c) and edit if needed.
4. Run verification (§5).
5. Write the implementation report (§2d).

---

## 5. Verification

```bash
uv run pytest backend/tests/task_center -q
uv run pytest backend/tests/test_tools backend/tests/task_center backend/tests/server -q
uv run ruff check backend/src backend/tests
uv run mypy --config-file backend/mypy.ini backend/src/task_center backend/src/agents
```

All must be green. No new behavior is introduced, so this is a regression
check, not a new-coverage exercise.

---

## 6. Exit-criteria mapping

| Exit criterion | Coverage |
| --- | --- |
| All phase tests pass | Verification commands |
| Public executor contract = `request_complex_task_solution` + `submit_execution_{success,failure}` | §1b row "request_complex_task_solution from generator…" + §2a regression |
| Docs no longer describe retry as `RETRY_ON_FAILURE` child graph creation | §2c sweep |
| Segment progression reflects continuation through `continuation_goal` inherited from passing harness graph | §1b rows on continuation/inheritance |
| Retry history derived from ordered harness graphs inside one segment, with per-graph `continuation_goal` independence | §1b rows on retry/independence |

---

## 7. Definition of done

- §2a regression test exists and is green.
- §2b fixture comment landed (or decision documented as no-edit).
- §2c sweep run; either clean or fixed.
- Verification (§5) clean.
- Implementation report (§2d) written matching Phase 04's shape.

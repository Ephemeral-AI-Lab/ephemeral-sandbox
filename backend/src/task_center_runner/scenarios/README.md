# Live E2E Scenarios

Concrete scenarios that drive the live-e2e harness. See
`docs/architecture/task_center/bridges.html` for the maintained runner and
artifact boundary.

## Layout

- `base.py` — `Scenario` protocol, `ScenarioBase`, `ScenarioContext`, `ToolCallSpec`.
- `_scenario_helpers/` — shared plan shapes, goal-origin predicates, and context-message token parsers.
- `pipeline/` — task_center state-machine scenarios (goal/iteration/attempt control flow).
- `sandbox/` — sandbox subsystem scenarios (OCC, overlay, layerstack, LSP, daemon).
- `capacity/` — composite scenarios that intentionally span multiple subsystem owners.
- `planner_validation/` — invalid plan rejection.
- `correctness_testing.py`, `full_case_user_input.py`, `full_stack_adversarial.py` —
  composite end-to-end scenarios (existing). Slated to move under `composite/`
  in a follow-up; left at the top level for now.

## Adding a scenario

1. Pick the right subpackage from the layout above.
2. Copy the closest reference scenario:
   - State-machine assertion → `pipeline/initial_workflow.py`
   - DAG dependency assertion → `pipeline/dependency_dag_serial.py`
   - Iterative continuation → `pipeline/iterative_deferral.py`
   - Attempt-retry assertion → `pipeline/attempt_retry_evaluator_failure.py`
   - Sandbox event assertion → `sandbox/occ_concurrent_conflicts.py`
   - Planner rejection assertion → `planner_validation/duplicate_local_id.py`
3. Update `SCENARIO_REGISTRY` in `__init__.py`.
4. Add paired smoke/full tests under the matching mock category,
   usually `backend/src/task_center_runner/tests/mock/sandbox/` for
   sandbox-heavy scenarios or
   `backend/src/task_center_runner/tests/mock/task_center/` for
   TaskCenter workflow correctness. Use `test_<scenario>_{smoke,full}.py` when
   the scenario has both profiles. Keep shared assertions in a private helper
   such as `_project_build_contracts.py` instead of using a `smoke` boolean in
   the test entrypoint.
5. Run `uv run pytest backend/src/task_center_runner/tests/mock/contracts/test_scenario_suite_imports.py -q`
   to verify protocol conformance and registry membership.

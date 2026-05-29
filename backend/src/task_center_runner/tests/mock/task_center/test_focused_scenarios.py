"""Live regressions for the focused scenario reference suite."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.audit.events import EventType
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from task_center_runner.tests._live_config import database_configured
from task_center_runner.tests.mock._focused_scenario_contracts import (
    FocusedScenarioCase,
    assert_focused_scenario_report,
)

pytestmark = pytest.mark.asyncio


_FOCUSED_CASES: tuple[FocusedScenarioCase, ...] = (
    FocusedScenarioCase(
        "pipeline.initial_workflow",
        min_event_counts={
            EventType.EXECUTOR_INVOKED: 1,
            EventType.EXECUTOR_SUCCESS: 1,
            EventType.EVALUATOR_SUCCESS: 1,
        },
        attempt_count=1,
    ),
    FocusedScenarioCase(
        "pipeline.iterative_deferral",
        min_event_counts={
            EventType.PLANNER_DEFERS_GOAL_PLAN: 1,
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 1,
            EventType.EXECUTOR_SUCCESS: 2,
            EventType.EVALUATOR_SUCCESS: 2,
        },
        iteration_count=2,
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "pipeline.attempt_retry_evaluator_failure",
        min_event_counts={
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 2,
            EventType.EXECUTOR_SUCCESS: 2,
            EventType.EVALUATOR_FAILURE: 1,
            EventType.EVALUATOR_SUCCESS: 1,
        },
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "pipeline.attempt_retry_planner_failure",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 1,
            EventType.TOOL_CALL_ERROR: 1,
            EventType.EXECUTOR_SUCCESS: 1,
            EventType.EVALUATOR_SUCCESS: 1,
        },
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "pipeline.attempt_retry_generator_failure",
        min_event_counts={
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 2,
            EventType.EXECUTOR_FAILURE: 1,
            EventType.EXECUTOR_SUCCESS: 1,
            EventType.EVALUATOR_SUCCESS: 1,
        },
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "pipeline.dependency_dag_serial",
        min_event_counts={
            EventType.EXECUTOR_INVOKED: 3,
            EventType.EXECUTOR_SUCCESS: 3,
        },
        attempt_count=1,
    ),
    FocusedScenarioCase(
        "pipeline.dependency_dag_mixed",
        min_event_counts={
            EventType.EXECUTOR_INVOKED: 7,
            EventType.EXECUTOR_SUCCESS: 7,
        },
        attempt_count=1,
    ),
    FocusedScenarioCase(
        "pipeline.dependency_dag_parallel",
        min_event_counts={
            EventType.EXECUTOR_INVOKED: 4,
            EventType.EXECUTOR_SUCCESS: 4,
        },
        attempt_count=1,
    ),
    FocusedScenarioCase(
        "pipeline.dependency_dag_diamond",
        min_event_counts={
            EventType.EXECUTOR_INVOKED: 4,
            EventType.EXECUTOR_SUCCESS: 4,
        },
        attempt_count=1,
    ),
    FocusedScenarioCase(
        "pipeline.generator_failure_quiescence",
        min_event_counts={
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 2,
            EventType.EXECUTOR_INVOKED: 7,
            EventType.EXECUTOR_SUCCESS: 6,
            EventType.EXECUTOR_FAILURE: 1,
            EventType.EVALUATOR_SUCCESS: 1,
        },
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "pipeline.dependency_blocked_descendants",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 2,
            EventType.EXECUTOR_INVOKED: 2,
            EventType.EXECUTOR_FAILURE: 2,
        },
        absent_events=(EventType.EVALUATOR_INVOKED,),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "pipeline.attempt_budget_exhausted",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_COMPLETES_GOAL_PLAN: 2,
            EventType.EXECUTOR_FAILURE: 2,
        },
        absent_events=(EventType.EVALUATOR_INVOKED,),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "planner_validation.duplicate_local_id",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.TOOL_CALL_ERROR: 2,
        },
        absent_events=(
            EventType.PLANNER_COMPLETES_GOAL_PLAN,
            EventType.EXECUTOR_INVOKED,
            EventType.EVALUATOR_INVOKED,
        ),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "planner_validation.unknown_dep",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.TOOL_CALL_ERROR: 2,
        },
        absent_events=(
            EventType.PLANNER_COMPLETES_GOAL_PLAN,
            EventType.EXECUTOR_INVOKED,
            EventType.EVALUATOR_INVOKED,
        ),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "planner_validation.cycle_in_deps",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.TOOL_CALL_ERROR: 2,
        },
        absent_events=(
            EventType.PLANNER_COMPLETES_GOAL_PLAN,
            EventType.EXECUTOR_INVOKED,
            EventType.EVALUATOR_INVOKED,
        ),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "planner_validation.defers_without_deferred_goal",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.TOOL_CALL_ERROR: 2,
        },
        absent_events=(
            EventType.PLANNER_COMPLETES_GOAL_PLAN,
            EventType.PLANNER_DEFERS_GOAL_PLAN,
            EventType.EXECUTOR_INVOKED,
            EventType.EVALUATOR_INVOKED,
        ),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "planner_validation.unknown_agent_name",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.TOOL_CALL_ERROR: 2,
        },
        absent_events=(
            EventType.PLANNER_COMPLETES_GOAL_PLAN,
            EventType.EXECUTOR_INVOKED,
            EventType.EVALUATOR_INVOKED,
        ),
        workflow_status="failed",
        attempt_count=2,
    ),
    FocusedScenarioCase(
        "planner_validation.empty_tasks",
        expected_status="failed",
        min_event_counts={
            EventType.PLANNER_INVOKED: 2,
            EventType.TOOL_CALL_ERROR: 2,
        },
        absent_events=(
            EventType.PLANNER_COMPLETES_GOAL_PLAN,
            EventType.EXECUTOR_INVOKED,
            EventType.EVALUATOR_INVOKED,
        ),
        workflow_status="failed",
        attempt_count=2,
    ),
)


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.parametrize("case", _FOCUSED_CASES, ids=[case.name for case in _FOCUSED_CASES])
async def test_focused_reference_scenario_runs(
    case: FocusedScenarioCase,
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY[case.name]()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert_focused_scenario_report(report, scenario, case)

"""Planner submission tool validation and routing tests."""

from __future__ import annotations

import pytest

from task_center.attempt import AttemptStage
from tools._framework.execution.tool_call import execute_tool_once
from tools.submission.planner import submit_plan_closes_goal, submit_plan_defers_goal

from .submission_test_utils import (
    build_harness_fixture,
    make_tool_context,
    start_planner,
)

pytestmark = pytest.mark.asyncio


async def _noop_emit(event) -> None:
    del event


def _valid_plan_payload() -> dict[str, object]:
    return {
        "plan_spec": "Implement the requested change.",
        "evaluation_criteria": ["tests pass"],
        "tasks": [
            {"id": "a", "agent_name": "executor", "deps": []},
            {"id": "b", "agent_name": "verifier", "deps": ["a"]},
        ],
        "task_specs": {
            "a": "Do the implementation.",
            "b": "Verify the implementation.",
        },
    }


async def test_full_plan_routes_to_apply_plan_submission(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)

    result = await execute_tool_once(
        submit_plan_closes_goal,
        _valid_plan_payload(),
        make_tool_context(
            fixture, planner_id, advisor_approves="submit_plan_closes_goal"
        ),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert not result.is_error
    assert result.is_terminal
    assert result.metadata["submission_kind"] == "planner_full"
    assert attempt is not None
    assert attempt.stage == AttemptStage.GENERATE
    assert attempt.generator_task_ids


async def test_partial_plan_routes_to_apply_plan_submission(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), "deferred_goal_for_next_iteration": "  continue with phase 2  "}

    result = await execute_tool_once(
        submit_plan_defers_goal,
        payload,
        make_tool_context(
            fixture, planner_id, advisor_approves="submit_plan_defers_goal"
        ),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert not result.is_error
    assert attempt is not None
    assert attempt.deferred_goal_for_next_iteration == "  continue with phase 2  "


@pytest.mark.parametrize(
    ("payload_update", "expected"),
    [
        (
            {
                "tasks": [
                    {"id": "a", "agent_name": "executor", "deps": []},
                    {"id": "a", "agent_name": "verifier", "deps": []},
                ],
            },
            "duplicate task id",
        ),
        (
            {
                "tasks": [{"id": "a", "agent_name": "missing", "deps": []}],
                "task_specs": {"a": "Do it."},
            },
            "Unknown generator agent",
        ),
        ({"task_specs": {"a": "Do it."}}, "Missing task_specs"),
        (
            {
                "task_specs": {
                    "a": "Do it.",
                    "b": "Check it.",
                    "c": "Extra.",
                },
            },
            "unknown ids",
        ),
        ({"task_specs": {"a": " ", "b": "Check it."}}, "must be nonblank"),
        (
            {
                "tasks": [{"id": "a", "agent_name": "executor", "deps": ["z"]}],
                "task_specs": {"a": "Do it."},
            },
            "unknown deps",
        ),
        (
            {
                "tasks": [
                    {"id": "a", "agent_name": "executor", "deps": ["b"]},
                    {"id": "b", "agent_name": "verifier", "deps": ["a"]},
                ],
            },
            "dependency cycle",
        ),
        (
            {
                "tasks": [{"id": " ", "agent_name": "executor", "deps": []}],
                "task_specs": {" ": "Do it."},
            },
            "id must be nonblank",
        ),
        (
            {"evaluation_criteria": [" "]},
            "evaluation_criteria must be nonblank",
        ),
    ],
)
async def test_plan_validation_errors_do_not_mutate_graph(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    composer,
    payload_update,
    expected,
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), **payload_update}

    result = await execute_tool_once(
        submit_plan_closes_goal,
        payload,
        make_tool_context(
            fixture, planner_id, advisor_approves="submit_plan_closes_goal"
        ),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert result.is_error
    assert expected in result.output
    assert attempt is not None
    assert attempt.stage == AttemptStage.PLAN


async def test_full_plan_rejects_deferred_goal(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), "deferred_goal_for_next_iteration": "continue later"}

    result = await execute_tool_once(
        submit_plan_closes_goal,
        payload,
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert result.is_error
    assert "deferred_goal_for_next_iteration" in result.output
    assert "Extra inputs are not permitted" in result.output
    assert attempt is not None
    assert attempt.stage == AttemptStage.PLAN


async def test_partial_plan_rejects_blank_deferred_goal(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), "deferred_goal_for_next_iteration": " "}

    result = await execute_tool_once(
        submit_plan_defers_goal,
        payload,
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert result.is_error
    assert "deferred_goal_for_next_iteration must be nonblank" in result.output
    assert attempt is not None
    assert attempt.stage == AttemptStage.PLAN

"""Planner submission tool validation and routing tests."""

from __future__ import annotations

import pytest

from task_center.attempt import AttemptStage
from tools._framework.execution.tool_call import execute_tool_once
from tools.submission.planner import submit_plan_closes_goal, submit_plan_defers_goal
from tools.submission.planner._prompt_guidance import (
    PLAN_DAG_GUIDANCE,
    PLAN_SUBMISSION_CHOICE_GUIDANCE,
)
from tools.submission.planner.submit_plan_closes_goal.prompt import (
    get_submit_plan_closes_goal_description,
)
from tools.submission.planner.submit_plan_defers_goal.prompt import (
    get_submit_plan_defers_goal_description,
)

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
        "tasks": [
            {"id": "a", "agent_name": "executor", "needs": []},
            {"id": "b", "agent_name": "generator", "needs": ["a"]},
        ],
        "task_specs": {
            "a": "Do the implementation.",
            "b": "Do the follow-up.",
        },
        "reducers": [
            {"id": "r", "needs": ["b"], "prompt": "Confirm the work is complete."},
        ],
    }


async def test_plan_tool_descriptions_share_dag_guidance() -> None:
    closes = get_submit_plan_closes_goal_description()
    defers = get_submit_plan_defers_goal_description()

    assert PLAN_DAG_GUIDANCE in closes
    assert PLAN_DAG_GUIDANCE in defers
    assert PLAN_SUBMISSION_CHOICE_GUIDANCE in closes
    assert PLAN_SUBMISSION_CHOICE_GUIDANCE in defers
    assert "The attempt PASSES iff every plan task reaches DONE." in closes
    assert "## Close vs Defer Decision" in closes
    assert "## Close vs Defer Decision" in defers
    assert "Lane shape does not decide close vs defer" in defers
    assert "outcomes become prior-iteration context" in defers
    assert "deferred_goal_for_next_iteration" in defers


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
    assert result.metadata["submission_kind"] == "planner_completes"
    assert attempt is not None
    assert attempt.stage == AttemptStage.RUN
    assert attempt.generator_task_ids
    assert attempt.reducer_task_ids


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
                    {"id": "a", "agent_name": "executor", "needs": []},
                    {"id": "a", "agent_name": "generator", "needs": []},
                ],
            },
            "duplicate task id",
        ),
        (
            {
                "tasks": [{"id": "a", "agent_name": "missing", "needs": []}],
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
                "tasks": [{"id": "a", "agent_name": "executor", "needs": ["z"]}],
                "task_specs": {"a": "Do it."},
            },
            "unknown needs",
        ),
        (
            {
                "tasks": [
                    {"id": "a", "agent_name": "executor", "needs": ["b"]},
                    {"id": "b", "agent_name": "generator", "needs": ["a"]},
                ],
            },
            "dependency cycle",
        ),
        (
            {
                "tasks": [
                    {"id": "a", "agent_name": "executor", "needs": []},
                    {"id": "b", "agent_name": "executor", "needs": []},
                ],
                "task_specs": {"a": "Do it.", "b": "Do b."},
                "reducers": [{"id": "r", "needs": ["a"], "prompt": "Gate a."}],
            },
            "no downstream task needs",
        ),
        (
            {
                "tasks": [{"id": "a", "agent_name": "executor", "needs": []}],
                "task_specs": {"a": "Do it."},
                "reducers": [{"id": "r", "needs": [], "prompt": "Gate it."}],
            },
            "must need at least one generator",
        ),
        (
            {
                "tasks": [
                    {"id": "a", "agent_name": "executor", "needs": []},
                    {"id": "b", "agent_name": "executor", "needs": ["r"]},
                ],
                "task_specs": {"a": "Do it.", "b": "Do b."},
                "reducers": [{"id": "r", "needs": ["a"], "prompt": "Gate a."}],
            },
            "cannot need reducer",
        ),
        (
            {
                "tasks": [
                    {"id": "a", "agent_name": "executor", "needs": []},
                    {"id": "b", "agent_name": "executor", "needs": ["a"]},
                ],
                "task_specs": {"a": "Do it.", "b": "Do b."},
                "reducers": [
                    {"id": "r1", "needs": ["b"], "prompt": "Gate b."},
                    {"id": "r2", "needs": ["r1"], "prompt": "Gate r1."},
                ],
            },
            "cannot need reducer",
        ),
        (
            {
                "tasks": [{"id": " ", "agent_name": "executor", "needs": []}],
                "task_specs": {" ": "Do it."},
            },
            "id must be nonblank",
        ),
        (
            {"reducers": []},
            "at least 1 item",
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

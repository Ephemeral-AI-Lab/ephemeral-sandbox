"""Planner submission tool validation and routing tests."""

from __future__ import annotations

import pytest

from task_center.attempt import HarnessGraphStage
from tools.core.tool_execution import execute_tool_once
from tools.submission.main_agent.planner import submit_full_plan, submit_partial_plan

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
        "task_specification": "Implement the requested change.",
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
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)

    result = await execute_tool_once(
        submit_full_plan,
        _valid_plan_payload(),
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert not result.is_error
    assert result.does_terminate
    assert result.metadata["submission_kind"] == "planner_full"
    assert graph is not None
    assert graph.stage == HarnessGraphStage.GENERATING
    assert graph.generator_task_ids


async def test_partial_plan_routes_to_apply_plan_submission(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), "continuation_goal": "  continue with phase 2  "}

    result = await execute_tool_once(
        submit_partial_plan,
        payload,
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert not result.is_error
    assert graph is not None
    assert graph.continuation_goal == "  continue with phase 2  "


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
    request_store,
    segment_store,
    graph_store,
    task_store,
    composer,
    payload_update,
    expected,
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), **payload_update}

    result = await execute_tool_once(
        submit_full_plan,
        payload,
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert result.is_error
    assert expected in result.output
    assert graph is not None
    assert graph.stage == HarnessGraphStage.PLANNING


async def test_full_plan_rejects_continuation_goal(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), "continuation_goal": "continue later"}

    result = await execute_tool_once(
        submit_full_plan,
        payload,
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert result.is_error
    assert "continuation_goal" in result.output
    assert "Extra inputs are not permitted" in result.output
    assert graph is not None
    assert graph.stage == HarnessGraphStage.PLANNING


async def test_partial_plan_rejects_blank_continuation_goal(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    planner_id = start_planner(fixture)
    payload = {**_valid_plan_payload(), "continuation_goal": " "}

    result = await execute_tool_once(
        submit_partial_plan,
        payload,
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert result.is_error
    assert "continuation_goal must be nonblank" in result.output
    assert graph is not None
    assert graph.stage == HarnessGraphStage.PLANNING

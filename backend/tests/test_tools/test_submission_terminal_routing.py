"""Terminal routing tests for generator and evaluator submissions."""

from __future__ import annotations

import pytest

from task_center.mission.mission import ComplexTaskRequestStatus
from task_center.attempt import HarnessGraphStage, HarnessGraphStatus
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    HarnessTaskStatus,
    PlannedGeneratorTask,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)
from tools.core.tool_execution import execute_tool_once
from tools.submission.main_agent.evaluator import (
    submit_evaluation_failure,
    submit_evaluation_success,
)
from tools.submission.main_agent.generator import request_complex_task_solution
from tools.submission.main_agent.generator.executor import (
    submit_execution_failure,
    submit_execution_success,
)
from tools.submission.main_agent.generator.verifier import (
    submit_verification_success,
)

from .submission_test_utils import (
    apply_single_generator_plan,
    build_harness_fixture,
    make_tool_context,
    spawn_evaluator,
)

pytestmark = pytest.mark.asyncio


async def _noop_emit(event) -> None:
    del event


async def test_submit_execution_success_calls_apply_generator_submission(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        submit_execution_success,
        {"summary": "done", "artifacts": ["artifact"]},
        make_tool_context(fixture, generator_id),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert result.does_terminate
    assert task is not None
    assert task["status"] == HarnessTaskStatus.DONE.value
    assert task["summaries"][-1]["payload"]["generator_role"] == "executor"


async def test_submit_execution_failure_calls_apply_generator_submission(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        submit_execution_failure,
        {"summary": "failed", "reason": "blocked", "details": ["detail"]},
        make_tool_context(fixture, generator_id),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert task is not None
    assert task["status"] == HarnessTaskStatus.FAILED.value
    assert task["summaries"][-1]["payload"]["reason"] == "blocked"


async def test_submit_verification_success_calls_apply_generator_submission(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture, agent_name="verifier")

    result = await execute_tool_once(
        submit_verification_success,
        {"summary": "verified", "checks": ["pytest"]},
        make_tool_context(fixture, generator_id, role="verifier"),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert task is not None
    assert task["summaries"][-1]["payload"]["generator_role"] == "verifier"


async def test_submit_evaluation_success_calls_apply_evaluator_submission(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    evaluator_id = spawn_evaluator(fixture)

    result = await execute_tool_once(
        submit_evaluation_success,
        {"summary": "passed", "passed_criteria": ["criterion"]},
        make_tool_context(fixture, evaluator_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert not result.is_error
    assert graph is not None
    assert graph.status == HarnessGraphStatus.PASSED


async def test_submit_evaluation_failure_calls_apply_evaluator_submission(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    evaluator_id = spawn_evaluator(fixture)

    result = await execute_tool_once(
        submit_evaluation_failure,
        {"summary": "failed", "failed_criteria": ["criterion"]},
        make_tool_context(fixture, evaluator_id),
        emit=_noop_emit,
    )

    graph = graph_store.get(fixture.graph_id)
    assert not result.is_error
    assert graph is not None
    assert graph.status == HarnessGraphStatus.FAILED


async def test_request_complex_task_solution_starts_delegated_request(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        request_complex_task_solution,
        {"goal": "solve delegated task"},
        make_tool_context(fixture, generator_id),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    delegated_request = request_store.get(result.metadata["complex_task_request_id"])
    initial_segment = segment_store.get(result.metadata["initial_segment_id"])
    created_harness_graph = graph_store.get(result.metadata["initial_harness_graph_id"])

    assert not result.is_error
    assert result.does_terminate
    assert task is not None
    assert task["status"] == HarnessTaskStatus.WAITING_COMPLEX_TASK.value
    assert delegated_request is not None
    assert delegated_request.status == ComplexTaskRequestStatus.OPEN
    assert delegated_request.requested_by_task_id == generator_id
    assert delegated_request.goal == "solve delegated task"
    assert initial_segment is not None
    assert initial_segment.complex_task_request_id == delegated_request.id
    assert created_harness_graph is not None
    assert created_harness_graph.task_segment_id == initial_segment.id
    assert created_harness_graph.stage == HarnessGraphStage.PLANNING


async def test_request_complex_task_solution_accepts_any_generator_agent_profile(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    from agents import registry as agents_registry
    from agents.types import AgentDefinition

    agents_registry.register_definition(
        AgentDefinition(
            name="custom_generator",
            description="custom generator for this test",
            role="generator",
            context_recipe="generator_v1",
            terminals=[
                "request_complex_task_solution",
                "submit_execution_success",
                "submit_execution_failure",
            ],
        )
    )

    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(
        fixture,
        agent_name="custom_generator",
    )

    result = await execute_tool_once(
        request_complex_task_solution,
        {"goal": "delegate broad custom generator work"},
        make_tool_context(fixture, generator_id),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert result.does_terminate
    assert task is not None
    assert task["status"] == HarnessTaskStatus.WAITING_COMPLEX_TASK.value


async def test_request_complex_task_solution_return_updates_outer_generator(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    outer_generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        request_complex_task_solution,
        {"goal": "solve delegated task"},
        make_tool_context(fixture, outer_generator_id),
        emit=_noop_emit,
    )
    delegated_graph_id = result.metadata["initial_harness_graph_id"]
    delegated_orchestrator = fixture.runtime.orchestrator_registry.get_or_raise(
        delegated_graph_id
    )
    delegated_planner_id = planner_task_id(delegated_graph_id)
    delegated_generator_id = generator_task_id(delegated_graph_id, "delegated")
    delegated_evaluator_id = evaluator_task_id(delegated_graph_id)

    delegated_orchestrator.apply_plan_submission(
        PlannerSubmission(
            graph_id=delegated_graph_id,
            planner_task_id=delegated_planner_id,
            kind="full",
            task_specification="Solve delegated task.",
            evaluation_criteria=("delegated task passed",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="delegated",
                    agent_name="executor",
                    deps=(),
                    task_spec="Do delegated work.",
                ),
            ),
            continuation_goal=None,
            summary="Accepted delegated plan.",
        )
    )
    delegated_orchestrator.apply_generator_submission(
        GeneratorSubmission(
            graph_id=delegated_graph_id,
            task_id=delegated_generator_id,
            outcome="success",
            summary="Delegated work done.",
            payload={},
        )
    )
    delegated_orchestrator.apply_evaluator_submission(
        EvaluatorSubmission(
            graph_id=delegated_graph_id,
            task_id=delegated_evaluator_id,
            outcome="success",
            summary="Delegated task passed.",
            payload={},
        )
    )

    outer_task = task_store.get_task(outer_generator_id)
    outer_graph = graph_store.get(fixture.graph_id)
    delegated_request = request_store.get(result.metadata["complex_task_request_id"])

    assert outer_task is not None
    assert outer_task["status"] == HarnessTaskStatus.DONE.value
    assert outer_task["summaries"][-1]["payload"]["complex_task_close_report"][
        "final_harness_graph_id"
    ] == delegated_graph_id
    assert outer_graph is not None
    assert outer_graph.stage == HarnessGraphStage.EVALUATING
    assert delegated_request is not None
    assert delegated_request.status == ComplexTaskRequestStatus.SUCCEEDED

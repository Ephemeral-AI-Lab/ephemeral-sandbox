"""Terminal routing tests for generator and evaluator submissions."""

from __future__ import annotations

import pytest

from task_center.workflow.state import WorkflowStatus
from task_center.attempt import AttemptStage, AttemptStatus
from task_center._core.task_state import TaskCenterTaskStatus
from task_center.submissions import (
    EvaluatorSubmission,
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannerSubmission,
)
from task_center._core.primitives import evaluator_task_id, generator_task_id, planner_task_id
from tools._framework.execution.tool_call import execute_tool_once
from tools.submission.evaluator import (
    submit_evaluation_failure,
    submit_evaluation_success,
)
from tools.submission.executor import submit_execution_handoff
from tools.submission.executor import (
    submit_execution_blocker,
    submit_execution_success,
)
from tools.submission.verifier import (
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
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        submit_execution_success,
        {"summary": "done", "artifacts": ["artifact"]},
        make_tool_context(
            fixture, generator_id, advisor_approves="submit_execution_success"
        ),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert result.is_terminal
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert task["summaries"][-1]["payload"]["generator_role"] == "executor"


async def test_submit_execution_blocker_calls_apply_generator_submission(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        submit_execution_blocker,
        {"summary": "blocked by missing dependency"},
        make_tool_context(
            fixture, generator_id, advisor_approves="submit_execution_blocker"
        ),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.BLOCKED.value
    assert task["summaries"][-1]["outcome"] == "blocker"


async def test_submit_verification_success_calls_apply_generator_submission(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture, agent_name="verifier")

    result = await execute_tool_once(
        submit_verification_success,
        {"summary": "verified", "checks": ["pytest"]},
        make_tool_context(
            fixture,
            generator_id,
            role="verifier",
            advisor_approves="submit_verification_success",
        ),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert task is not None
    assert task["summaries"][-1]["payload"]["generator_role"] == "verifier"


async def test_submit_evaluation_success_calls_apply_evaluator_submission(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    evaluator_id = spawn_evaluator(fixture)

    result = await execute_tool_once(
        submit_evaluation_success,
        {"summary": "passed", "passed_criteria": ["criterion"]},
        make_tool_context(
            fixture, evaluator_id, advisor_approves="submit_evaluation_success"
        ),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert not result.is_error
    assert attempt is not None
    assert attempt.status == AttemptStatus.PASSED


async def test_submit_evaluation_failure_calls_apply_evaluator_submission(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    evaluator_id = spawn_evaluator(fixture)

    result = await execute_tool_once(
        submit_evaluation_failure,
        {"summary": "failed", "failed_criteria": ["criterion"]},
        make_tool_context(
            fixture, evaluator_id, advisor_approves="submit_evaluation_failure"
        ),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(fixture.attempt_id)
    assert not result.is_error
    assert attempt is not None
    assert attempt.status == AttemptStatus.FAILED


async def test_submit_execution_handoff_starts_delegated_request(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        submit_execution_handoff,
        {"goal_handoff": "solve delegated task"},
        make_tool_context(
            fixture, generator_id, advisor_approves="submit_execution_handoff"
        ),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    delegated_request = workflow_store.get(result.metadata["workflow_id"])
    initial_iteration = iteration_store.get(result.metadata["initial_iteration_id"])
    created_attempt = attempt_store.get(result.metadata["initial_attempt_id"])

    assert not result.is_error
    assert result.is_terminal
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.WAITING_WORKFLOW.value
    assert delegated_request is not None
    assert delegated_request.status == WorkflowStatus.OPEN
    assert delegated_request.requested_by_task_id == generator_id
    assert delegated_request.goal == "solve delegated task"
    assert initial_iteration is not None
    assert initial_iteration.workflow_id == delegated_request.id
    assert created_attempt is not None
    assert created_attempt.iteration_id == initial_iteration.id
    assert created_attempt.stage == AttemptStage.PLAN


async def test_submit_execution_handoff_accepts_any_generator_agent_profile(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    from agents import AgentDefinition, AgentKind, register_definition

    register_definition(
        AgentDefinition(
            name="custom_generator",
            description="custom generator for this test",
            tool_call_limit=10,
            agent_kind=AgentKind.EXECUTOR,
            dispatchable_by_planner=True,
            context_recipe="generator",
            terminals=[
                "submit_execution_handoff",
                "submit_execution_success",
                "submit_execution_blocker",
            ],
        )
    )

    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    generator_id = apply_single_generator_plan(
        fixture,
        agent_name="custom_generator",
    )

    result = await execute_tool_once(
        submit_execution_handoff,
        {"goal_handoff": "delegate broad custom generator work"},
        make_tool_context(
            fixture, generator_id, advisor_approves="submit_execution_handoff"
        ),
        emit=_noop_emit,
    )

    task = task_store.get_task(generator_id)
    assert not result.is_error
    assert result.is_terminal
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.WAITING_WORKFLOW.value


async def test_submit_execution_handoff_return_updates_outer_generator(
    workflow_store, iteration_store, attempt_store, task_store, composer
) -> None:
    fixture = build_harness_fixture(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        composer=composer,
    )
    outer_generator_id = apply_single_generator_plan(fixture)

    result = await execute_tool_once(
        submit_execution_handoff,
        {"goal_handoff": "solve delegated task"},
        make_tool_context(
            fixture,
            outer_generator_id,
            advisor_approves="submit_execution_handoff",
        ),
        emit=_noop_emit,
    )
    delegated_attempt_id = result.metadata["initial_attempt_id"]
    delegated_orchestrator = fixture.runtime.orchestrator_registry.get_or_raise(
        delegated_attempt_id
    )
    delegated_planner_id = planner_task_id(delegated_attempt_id)
    delegated_generator_id = generator_task_id(delegated_attempt_id, "delegated")
    delegated_evaluator_id = evaluator_task_id(delegated_attempt_id)

    delegated_orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=delegated_attempt_id,
            planner_task_id=delegated_planner_id,
            kind="completes",
            plan_spec="Solve delegated task.",
            evaluation_criteria=("delegated task passed",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="delegated",
                    agent_name="executor",
                    deps=(),
                    task_spec="Do delegated work.",
                ),
            ),
            deferred_goal_for_next_iteration=None,
            summary="Accepted delegated plan.",
        )
    )
    delegated_orchestrator.apply_generator_submission(
        GeneratorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=delegated_generator_id,
            outcome="success",
            summary="Delegated work done.",
            payload={},
        )
    )
    delegated_orchestrator.apply_evaluator_submission(
        EvaluatorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=delegated_evaluator_id,
            outcome="success",
            summary="Delegated task passed.",
            payload={},
        )
    )

    outer_task = task_store.get_task(outer_generator_id)
    outer_attempt = attempt_store.get(fixture.attempt_id)
    delegated_request = workflow_store.get(result.metadata["workflow_id"])

    assert outer_task is not None
    assert outer_task["status"] == TaskCenterTaskStatus.DONE.value
    assert outer_task["summaries"][-1]["payload"]["workflow_closure_report"][
        "final_attempt_id"
    ] == delegated_attempt_id
    assert outer_attempt is not None
    assert outer_attempt.stage == AttemptStage.EVALUATE
    assert delegated_request is not None
    assert delegated_request.status == WorkflowStatus.SUCCEEDED

"""Hard gate tests for Phase 03 submission tools."""

from __future__ import annotations

import pytest

from message.messages import ConversationMessage, ToolResultBlock, ToolUseBlock
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.episode.episode import TaskSegmentCreationReason
from task_center.task import planner_task_id
from tools.core.tool_execution import execute_tool_once
from tools.submission.hooks.request_complex_task_before_edit_gate import (
    RequestComplexTaskBeforeEditGate,
)
from tools.submission.hooks.resolver_success_limit_gate import (
    ResolverSuccessLimitGate,
)
from tools.submission.main_agent.generator import request_complex_task_solution
from tools.submission.main_agent.generator.executor import (
    submit_execution_success,
)
from tools.submission.main_agent.generator.verifier import (
    submit_verification_failure,
    submit_verification_success,
)
from tools.submission.main_agent.planner import submit_partial_plan

from .submission_test_utils import (
    apply_single_generator_plan,
    build_harness_fixture,
    make_tool_context,
    start_planner,
)

pytestmark = pytest.mark.asyncio


async def _noop_emit(event) -> None:
    del event


def _edit_messages() -> list[ConversationMessage]:
    return [
        ConversationMessage(
            role="assistant",
            content=[ToolUseBlock(id="toolu_edit", name="edit_file", input={})],
        )
    ]


def _resolver_messages(count: int) -> list[ConversationMessage]:
    messages: list[ConversationMessage] = []
    for index in range(count):
        tool_id = f"toolu_resolver_{index}"
        messages.append(
            ConversationMessage(
                role="assistant",
                content=[ToolUseBlock(id=tool_id, name="ask_resolver", input={})],
            )
        )
        messages.append(
            ConversationMessage(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id=tool_id,
                        content="not resolved",
                        metadata={"resolver": {"resolved": False}},
                    )
                ],
            )
        )
    return messages


async def test_request_complex_task_solution_blocks_after_edit() -> None:
    gate = RequestComplexTaskBeforeEditGate()
    outcome = await gate.run(
        request_complex_task_solution.input_model(goal="delegated"),
        make_tool_context_stub(messages=_edit_messages()),
    )

    assert outcome.status == "fail"
    assert "disabled after the first edit" in outcome.reason


async def test_request_complex_task_solution_allows_before_edit() -> None:
    gate = RequestComplexTaskBeforeEditGate()
    tool_input = request_complex_task_solution.input_model(goal="delegated")
    outcome = await gate.run(tool_input, make_tool_context_stub(messages=[]))

    assert outcome.status == "pass"
    assert outcome.value == tool_input


async def test_resolver_success_gate_boundary_and_limit() -> None:
    gate = ResolverSuccessLimitGate("submit_verification_success")
    tool_input = submit_verification_success.input_model(summary="ok", checks=[])

    boundary = await gate.run(
        tool_input,
        make_tool_context_stub(messages=_resolver_messages(4)),
    )
    blocked = await gate.run(
        tool_input,
        make_tool_context_stub(messages=_resolver_messages(5)),
    )

    assert boundary.status == "pass"
    assert blocked.status == "fail"
    assert "five unresolved resolver calls" in blocked.reason


async def test_resolver_success_gate_does_not_block_failure_terminal(
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
        submit_verification_failure,
        {"summary": "failed", "unresolved_issues": ["still broken"]},
        make_tool_context(
            fixture, generator_id, messages=_resolver_messages(5), role="verifier"
        ),
        emit=_noop_emit,
    )

    assert not result.is_error
    assert result.does_terminate


async def test_role_gate_blocks_wrong_task_role(
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
        submit_execution_success,
        {"summary": "done", "artifacts": []},
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    assert result.is_error
    assert "generator tasks" in result.output


async def test_role_gate_blocks_missing_orchestrator(
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
    fixture.runtime.orchestrator_registry.deregister(fixture.graph_id)

    result = await execute_tool_once(
        submit_partial_plan,
        {
            "task_specification": "spec",
            "evaluation_criteria": ["ok"],
            "tasks": [{"id": "a", "agent_name": "executor", "deps": []}],
            "task_specs": {"a": "do it"},
            "continuation_goal": "continue",
        },
        make_tool_context(fixture, planner_id),
        emit=_noop_emit,
    )

    assert result.is_error
    assert "No active HarnessGraphOrchestrator" in result.output


async def test_partial_plan_ancestor_gate_allows_same_request_continuation(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    from datetime import UTC, datetime

    fixture = build_harness_fixture(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        composer=composer,
    )
    segment_store.set_continuation_goal(fixture.segment_id, "continue")
    # Populate seg-1's denormalized task_specification + task_summary so the
    # planner_v1 recipe sees a complete prior-segment chain when planning seg-2.
    segment_store.close_succeeded(
        fixture.segment_id,
        task_specification="seg-1 spec",
        task_summary="seg-1 summary",
        closed_at=datetime.now(UTC),
    )
    segment2 = segment_store.insert(
        complex_task_request_id=fixture.request_id,
        sequence_no=2,
        creation_reason=TaskSegmentCreationReason.PARTIAL_CONTINUATION,
        goal="next segment",
        attempt_budget=2,
    )
    request_store.append_segment_id(fixture.request_id, segment2.id)
    graph2 = graph_store.insert(task_segment_id=segment2.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment2.id, graph2.id)
    orchestrator2 = HarnessGraphOrchestrator(
        harness_graph=graph2,
        on_graph_closed=lambda graph_id: None,
        runtime=fixture.runtime,
    )
    fixture.runtime.orchestrator_registry.register(orchestrator2)
    orchestrator2.start()
    planner_id = planner_task_id(graph2.id)

    result = await execute_tool_once(
        submit_partial_plan,
        {
            "task_specification": "spec",
            "evaluation_criteria": ["ok"],
            "tasks": [{"id": "a", "agent_name": "executor", "deps": []}],
            "task_specs": {"a": "do it"},
            "continuation_goal": "continue again",
        },
        make_tool_context_for_graph(fixture, graph2.id, planner_id),
        emit=_noop_emit,
    )

    assert not result.is_error


def make_tool_context_stub(*, messages: list[ConversationMessage]):
    from tools.core.context import ToolExecutionContextService
    from tools.core.runtime import ExecutionMetadata

    return ToolExecutionContextService(
        cwd="/tmp",
        services=ExecutionMetadata(conversation_messages=messages),
    )


def make_tool_context_for_graph(fixture, graph_id: str, task_id: str):
    from tools.core.context import ToolExecutionContextService
    from tools.core.runtime import ExecutionMetadata

    return ToolExecutionContextService(
        cwd="/tmp",
        services=ExecutionMetadata(
            task_center_task_id=task_id,
            task_center_harness_graph_id=graph_id,
            harness_graph_runtime=fixture.runtime,
        ),
    )

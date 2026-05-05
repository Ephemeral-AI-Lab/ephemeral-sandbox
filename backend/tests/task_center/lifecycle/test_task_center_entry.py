"""TaskCenter-backed server entry tests.

The entry executor is graph-less (per phase-06 *Sources of truth*: an entry
segment may have zero ``HarnessGraph`` rows). These tests pin:

    - the entry task row writes ``task_center_harness_graph_id=None``;
    - the entry segment exists but contains zero ``HarnessGraph`` rows;
    - run exhaustion via :class:`EntryTaskController` finishes the run and
      closes the entry segment + complex_request.
"""

from __future__ import annotations

import pytest

from agents.registry import get_definition, register_definition, unregister_definition
from agents.types import AgentDefinition
from engine.runtime.lifecycle import EphemeralRunResult
from server.app_factory import RuntimeConfig
from task_center.entry import start_task_center_entry_run
from task_center.sandbox_bridge import TaskCenterSandboxBridge


def _fake_sandbox_bridge() -> TaskCenterSandboxBridge:
    return TaskCenterSandboxBridge(create_fn=lambda **_: {"id": "sb-entry-test"})


@pytest.mark.asyncio
async def test_entry_executor_runs_in_graph_less_mode(
    request_store,
    segment_store,
    graph_store,
    task_store,
    context_packet_store,
    tmp_path,
) -> None:
    previous = {
        name: get_definition(name)
        for name in ("entry_executor", "executor", "planner")
    }
    register_definition(
        AgentDefinition(
            name="entry_executor",
            description="test entry executor",
            role="executor",
            context_recipe="entry_executor_v1",
            terminals=["submit_execution_success", "submit_execution_failure"],
        )
    )
    register_definition(
        AgentDefinition(
            name="planner",
            description="test planner",
            role="planner",
            terminals=["submit_full_plan", "submit_partial_plan"],
        )
    )
    captured: list[dict[str, object]] = []

    async def fake_runner(*args, **kwargs):
        captured.append({**kwargs, "input_query": args[1]})
        agent_def = kwargs["agent_def"]
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=None,
            agent_name=agent_def.name,
            event_count=0,
        )

    try:
        entry = start_task_center_entry_run(
            config=RuntimeConfig(cwd=str(tmp_path)),
            prompt="do a complex thing",
            sandbox_id=None,
            on_agent_event=None,
            task_store=task_store,
            request_store=request_store,
            segment_store=segment_store,
            graph_store=graph_store,
            context_packet_store=context_packet_store,
            runner=fake_runner,
            sandbox_bridge=_fake_sandbox_bridge(),
        )
        await entry.launcher.wait_for_idle()
    finally:
        for name, definition in previous.items():
            unregister_definition(name)
            if definition is not None:
                register_definition(definition)

    request = request_store.get(entry.complex_task_request_id)
    task = task_store.get_task(entry.entry_task_id)
    run = task_store.get_run(entry.task_center_run_id)
    persisted_request = task_store.get_request(entry.request_id)
    assert request is not None
    assert entry.sandbox_id == "sb-entry-test"
    assert persisted_request is not None
    assert persisted_request["sandbox_id"] == "sb-entry-test"
    assert request.requested_by_task_id == entry.entry_task_id
    assert task is not None
    assert task["role"] == "generator"
    assert task["agent_name"] == "entry_executor"
    # Carve-out invariant: entry task is graph-less.
    assert task["task_center_harness_graph_id"] is None
    assert task["context_packet_id"] is not None
    packet = context_packet_store.get(task["context_packet_id"])
    assert packet is not None
    assert packet.blocks[0].kind == "entry_request"
    # Run finalization happens via the controller's apply_run_exhausted path
    # because the fake runner returns "completed" without a terminal.
    assert run is not None
    assert run["status"] == "failed"
    assert captured[0]["agent_def"].name == "entry_executor"
    assert "# Entry request" in captured[0]["input_query"]
    assert "do a complex thing" in captured[0]["input_query"]
    assert captured[0]["extra_tool_metadata"].task_center_task_id == entry.entry_task_id
    # Entry-mode tasks have no graph id but the runtime is always attached
    # so executor-shaped submissions can resolve through the unified
    # ``resolve_executor_submission_context`` path.
    assert captured[0]["extra_tool_metadata"].task_center_harness_graph_id is None
    assert captured[0]["extra_tool_metadata"].harness_graph_runtime is not None


@pytest.mark.asyncio
async def test_entry_segment_has_zero_harness_graph_rows(
    request_store,
    segment_store,
    graph_store,
    task_store,
    context_packet_store,
    tmp_path,
) -> None:
    """Regression: confirm the carve-out — entry segment contains 0 graphs."""
    previous = {
        name: get_definition(name)
        for name in ("entry_executor", "planner")
    }
    register_definition(
        AgentDefinition(
            name="entry_executor",
            description="test entry executor",
            role="executor",
            context_recipe="entry_executor_v1",
            terminals=["submit_execution_success", "submit_execution_failure"],
        )
    )
    register_definition(
        AgentDefinition(
            name="planner",
            description="test planner",
            role="planner",
            terminals=["submit_full_plan", "submit_partial_plan"],
        )
    )

    async def fake_runner(*args, **kwargs):
        del args
        agent_def = kwargs["agent_def"]
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=None,
            agent_name=agent_def.name,
            event_count=0,
        )

    try:
        entry = start_task_center_entry_run(
            config=RuntimeConfig(cwd=str(tmp_path)),
            prompt="prompt",
            sandbox_id=None,
            on_agent_event=None,
            task_store=task_store,
            request_store=request_store,
            segment_store=segment_store,
            graph_store=graph_store,
            context_packet_store=context_packet_store,
            runner=fake_runner,
            sandbox_bridge=_fake_sandbox_bridge(),
        )
        await entry.launcher.wait_for_idle()
    finally:
        for name, definition in previous.items():
            unregister_definition(name)
            if definition is not None:
                register_definition(definition)

    # Phase-06 *Sources of truth*: an entry segment may have zero HarnessGraph rows.
    assert graph_store.list_for_segment(entry.task_segment_id) == []
    segment = segment_store.get(entry.task_segment_id)
    assert segment is not None
    assert segment.harness_graph_ids == ()


@pytest.mark.asyncio
async def test_entry_executor_submit_execution_success_finishes_run(
    request_store,
    segment_store,
    graph_store,
    task_store,
    context_packet_store,
    tmp_path,
) -> None:
    """E2E: an agent that calls ``submit_execution_success`` from the entry
    executor must successfully resolve the unified executor submission
    context, transition the entry task to DONE, close the entry segment +
    request, and finish the run as 'done'.

    This pins the contract that the launcher attaches the runtime to entry
    launches even though there is no harness graph — without it, the
    unified ``resolve_executor_submission_context`` would error on a missing
    runtime and the entry executor could never call any of its terminals.
    """
    from pathlib import Path

    from db.stores.complex_task_request_store import ComplexTaskRequestStore  # noqa: F401
    from task_center.mission.mission import ComplexTaskRequestStatus
    from task_center.episode.episode import TaskSegmentStatus
    from task_center.task import HarnessTaskStatus
    from tools.core.context import ToolExecutionContextService
    from tools.core.tool_execution import execute_tool_once
    from tools.submission.main_agent.generator.executor import (
        submit_execution_success,
    )

    previous = {
        name: get_definition(name)
        for name in ("entry_executor", "planner")
    }
    register_definition(
        AgentDefinition(
            name="entry_executor",
            description="test entry executor",
            role="executor",
            context_recipe="entry_executor_v1",
            terminals=["submit_execution_success", "submit_execution_failure"],
        )
    )
    register_definition(
        AgentDefinition(
            name="planner",
            description="test planner",
            role="planner",
            terminals=["submit_full_plan", "submit_partial_plan"],
        )
    )

    async def _noop_emit(event):
        del event

    async def runner_that_submits_success(*args, **kwargs):
        del args
        agent_def = kwargs["agent_def"]
        metadata = kwargs["extra_tool_metadata"]
        # Real ``run_ephemeral_agent`` injects the agent's profile role into
        # the tool metadata before tool dispatch (engine.runtime.agent line
        # 353). Mirror that here so the ``HarnessAgentProfileGate`` accepts
        # the call.
        metadata = metadata.with_overrides(role=agent_def.role)
        context = ToolExecutionContextService(
            cwd=Path("/tmp"), services=metadata
        )
        result = await execute_tool_once(
            submit_execution_success,
            {"summary": "all good", "artifacts": ["a.md"]},
            context,
            emit=_noop_emit,
        )
        assert not result.is_error, result.output
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=result,
            agent_name=agent_def.name,
            event_count=1,
        )

    try:
        entry = start_task_center_entry_run(
            config=RuntimeConfig(cwd=str(tmp_path)),
            prompt="finish me cleanly",
            sandbox_id=None,
            on_agent_event=None,
            task_store=task_store,
            request_store=request_store,
            segment_store=segment_store,
            graph_store=graph_store,
            context_packet_store=context_packet_store,
            runner=runner_that_submits_success,
            sandbox_bridge=_fake_sandbox_bridge(),
        )
        await entry.launcher.wait_for_idle()
    finally:
        for name, definition in previous.items():
            unregister_definition(name)
            if definition is not None:
                register_definition(definition)

    task = task_store.get_task(entry.entry_task_id)
    fresh_segment = segment_store.get(entry.task_segment_id)
    fresh_request = request_store.get(entry.complex_task_request_id)
    run = task_store.get_run(entry.task_center_run_id)
    assert task is not None
    assert task["status"] == HarnessTaskStatus.DONE.value
    assert fresh_segment is not None
    assert fresh_segment.status == TaskSegmentStatus.SUCCEEDED
    assert fresh_request is not None
    assert fresh_request.status == ComplexTaskRequestStatus.SUCCEEDED
    assert run is not None
    assert run["status"] == "done"
    # And no synthetic graph appeared along the way.
    assert graph_store.list_for_segment(entry.task_segment_id) == []

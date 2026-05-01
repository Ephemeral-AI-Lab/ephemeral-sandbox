"""TaskCenter-backed server entry tests."""

from __future__ import annotations

import pytest

from agents.registry import get_definition, register_definition, unregister_definition
from agents.types import AgentDefinition
from engine.runtime.lifecycle import EphemeralRunResult
from server.app_factory import RuntimeConfig
from task_center.entry import start_task_center_entry_run


@pytest.mark.asyncio
async def test_entry_executor_runs_inside_task_center_graph(
    request_store,
    segment_store,
    graph_store,
    task_store,
    tmp_path,
) -> None:
    previous = {name: get_definition(name) for name in ("executor", "planner")}
    register_definition(
        AgentDefinition(
            name="executor",
            description="test executor",
            role="executor",
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
        del args
        captured.append(kwargs)
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
            runner=fake_runner,
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
    assert request is not None
    assert request.requested_by_task_id == entry.entry_task_id
    assert task is not None
    assert task["role"] == "generator"
    assert task["agent_name"] == "executor"
    assert task["task_center_harness_graph_id"] == entry.harness_graph_id
    assert run is not None
    assert run["status"] == "failed"
    assert captured[0]["agent_def"].name == "executor"
    assert captured[0]["extra_tool_metadata"].task_center_task_id == entry.entry_task_id

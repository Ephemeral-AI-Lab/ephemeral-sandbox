"""Unit coverage for mock probe queue-bridge helpers."""

from __future__ import annotations

import asyncio

import pytest

from task_center_runner.agent.mock.probe_bridge import _CallToolBridge
from tools._framework.core.results import ToolResult


@pytest.mark.asyncio
async def test_bridge_translates_requested_background_id_for_cancel() -> None:
    bridge = _CallToolBridge()
    bridge._background_aliases["requested-bg"] = "bg_2"  # noqa: SLF001

    pending = asyncio.create_task(
        bridge._call_loop_tool(  # noqa: SLF001
            "cancel_background_task",
            {"task_id": "requested-bg", "reason": "done"},
        )
    )
    kind, tool_name, raw_input, future = await bridge._queue.get()  # noqa: SLF001

    assert kind == "call"
    assert tool_name == "cancel_background_task"
    assert raw_input == {"task_id": "bg_2", "reason": "done"}

    future.set_result(ToolResult(output="cancelled", is_error=False))
    result = await pending

    assert result.output == "cancelled"


@pytest.mark.asyncio
async def test_bridge_background_await_polls_without_wait_turn() -> None:
    bridge = _CallToolBridge()

    pending = asyncio.create_task(
        bridge._await_background_result(task_id="bg_1", allow_error=True)  # noqa: SLF001
    )
    kind, tool_name, raw_input, future = await bridge._queue.get()  # noqa: SLF001

    assert kind == "call"
    assert tool_name == "check_background_task_result"
    assert raw_input == {"task_id": "bg_1"}

    future.set_result(
        ToolResult(
            output='{"id": "bg_1", "status": "running", "result": "[started]"}',
            is_error=False,
        )
    )
    await asyncio.sleep(0)
    assert bridge._queue.empty()  # noqa: SLF001

    kind, tool_name, raw_input, future = await asyncio.wait_for(
        bridge._queue.get(), 0.2  # noqa: SLF001
    )
    assert kind == "call"
    assert tool_name == "check_background_task_result"
    assert raw_input == {"task_id": "bg_1"}

    future.set_result(
        ToolResult(
            output=(
                '{"id": "bg_1", "status": "finished", '
                '"result": "{\\"status\\": \\"ok\\"}"}'
            ),
            is_error=False,
        )
    )
    result = await pending

    assert not result.is_error

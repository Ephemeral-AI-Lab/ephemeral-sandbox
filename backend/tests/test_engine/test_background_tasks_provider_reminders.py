"""Tests provider-backed reminder diffs for BackgroundTaskManager."""

from __future__ import annotations

import asyncio

from engine.runtime.background_tasks import BackgroundTaskManager
from tools.core.base import ToolResult


async def _slow_tool() -> ToolResult:
    await asyncio.sleep(5)
    return ToolResult(output="done")


async def test_get_reminder_diff_uses_progress_provider_deltas() -> None:
    mgr = BackgroundTaskManager()
    task_id = mgr.next_alias()
    snapshots = [
        "A: [text] first",
        "A: [text] first\nA: [text] second",
    ]
    idx = 0

    def provider(_: int) -> str:
        return snapshots[idx]

    mgr.launch(task_id, "run_subagent", {}, _slow_tool())
    mgr.set_progress_provider(task_id, provider)

    try:
        first_lines, _ = mgr.get_reminder_diff(task_id)
        assert first_lines == ["A: [text] first"]

        idx = 1
        second_lines, _ = mgr.get_reminder_diff(task_id)
        assert second_lines == ["A: [text] second"]
    finally:
        await mgr.cancel(task_id, "")

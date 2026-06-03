"""Engine/daemon in-flight invocation lifecycle contracts."""

from __future__ import annotations

import asyncio

import pytest

from engine.background.task_supervisor import BackgroundTaskSupervisor
from sandbox.daemon.rpc.in_flight import InFlightInvocationRegistry
from tools._framework.core.results import ToolResult


pytestmark = pytest.mark.asyncio


async def test_wire_cancel_precedes_local_task_cancel(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events: list[str] = []
    manager = BackgroundTaskSupervisor()

    async def sandbox_cancel(sandbox_id: str, invocation_id: str) -> dict[str, object]:
        events.append(f"wire:{sandbox_id}:{invocation_id}")
        await asyncio.sleep(0)
        return {"success": True}

    async def long_running() -> ToolResult:
        try:
            await asyncio.sleep(60)
        except asyncio.CancelledError:
            events.append("local-cancel")
            raise

    import sandbox.api as sandbox_api

    monkeypatch.setattr(sandbox_api, "cancel", sandbox_cancel)
    manager.launch(
        "bg_1",
        "shell",
        {},
        long_running(),
        agent_id="agent-a",
        uses_sandbox=True,
        sandbox_id="sandbox-1",
        sandbox_invocation_id="invocation-1",
    )

    assert await manager.cancel("bg_1", reason="test") is True
    task = manager.get_task("bg_1").asyncio_task  # type: ignore[union-attr]
    await asyncio.gather(task, return_exceptions=True)

    assert events == ["wire:sandbox-1:invocation-1", "local-cancel"]


async def test_heartbeat_reaper_cancels_stale_background_only() -> None:
    foreground_task = asyncio.create_task(asyncio.sleep(60))
    background_task = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=0.1, reaper_interval_s=60)
    registry.register(
        "foreground",
        foreground_task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=False,
    )
    registry.register(
        "background",
        background_task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=True,
    )
    registry._by_invocation["foreground"].last_seen -= 1.0
    registry._by_invocation["background"].last_seen -= 1.0

    registry.reap_stale()

    assert not foreground_task.cancelled()
    await asyncio.gather(background_task, return_exceptions=True)
    assert background_task.cancelled()
    assert registry.metrics()["ttl_reaped_total"] == 1

    foreground_task.cancel()
    await asyncio.gather(foreground_task, return_exceptions=True)

"""Unit tests for invocation-keyed daemon in-flight lifecycle."""

from __future__ import annotations

import asyncio

import pytest

from sandbox.daemon import builtin_operations as cancel_handler
from sandbox.daemon.rpc.in_flight import InFlightInvocationRegistry


pytestmark = pytest.mark.asyncio


async def test_cancel_task_cancels_registered_invocation() -> None:
    task = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=60, reaper_interval_s=60)
    registry.register(
        "invocation-1",
        task,
        agent_id="agent-a",
        op="api.v1.exec_command",
    )

    assert registry.cancel_task("invocation-1") is task
    await asyncio.gather(task, return_exceptions=True)
    assert task.cancelled()


async def test_heartbeat_refreshes_and_count_by_agent() -> None:
    foreground_task = asyncio.create_task(asyncio.sleep(60))
    background_task = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=60, reaper_interval_s=60)
    registry.register(
        "foreground-invocation",
        foreground_task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=False,
    )
    registry.register(
        "background-invocation",
        background_task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=True,
    )

    assert registry.count_by_agent("agent-a") == 1
    assert registry.heartbeat(["background-invocation"]) == 1
    assert registry.heartbeat(["missing"]) == 0

    foreground_task.cancel()
    background_task.cancel()
    await asyncio.gather(foreground_task, background_task, return_exceptions=True)


async def test_ttl_reaper_cancels_stale_invocation() -> None:
    task = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=0.1, reaper_interval_s=60)
    registry.register(
        "invocation-1",
        task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=True,
    )
    registry._by_invocation["invocation-1"].last_seen -= 1.0  # noqa: SLF001

    registry.reap_stale()
    assert registry.metrics() == {"active_invocations": 1, "ttl_reaped_total": 1}
    assert registry.count_by_agent("agent-a") == 1

    await asyncio.gather(task, return_exceptions=True)

    assert task.cancelled()
    registry.deregister("invocation-1")
    assert registry.metrics() == {"active_invocations": 0, "ttl_reaped_total": 1}


async def test_ttl_reaper_ignores_foreground_invocation() -> None:
    task = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=0.1, reaper_interval_s=60)
    registry.register(
        "invocation-1",
        task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=False,
    )
    registry._by_invocation["invocation-1"].last_seen -= 1.0  # noqa: SLF001

    registry.reap_stale()

    assert not task.cancelled()
    assert registry.metrics() == {"active_invocations": 1, "ttl_reaped_total": 0}
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)


async def test_env_overrides_use_positive_values(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EOS_INFLIGHT_TTL_S", "10")
    monkeypatch.setenv("EOS_INFLIGHT_REAPER_INTERVAL_S", "2")

    registry = InFlightInvocationRegistry()

    assert registry._ttl_seconds == 10.0  # noqa: SLF001
    assert registry._reaper_interval_s == 2.0  # noqa: SLF001


async def test_bad_env_overrides_fall_back_to_defaults(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("EOS_INFLIGHT_TTL_S", "-1")
    monkeypatch.setenv("EOS_INFLIGHT_REAPER_INTERVAL_S", "not-a-number")

    registry = InFlightInvocationRegistry()

    assert registry._ttl_seconds == 300.0  # noqa: SLF001
    assert registry._reaper_interval_s == 30.0  # noqa: SLF001


async def test_cancel_handler_targets_payload_invocation_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    task = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=60, reaper_interval_s=60)
    registry.register(
        "target-invocation",
        task,
        agent_id="agent-a",
        op="api.v1.exec_command",
    )
    monkeypatch.setattr(
        "sandbox.daemon.builtin_operations.get_in_flight_registry",
        lambda: registry,
    )

    response = await cancel_handler.cancel({"invocation_id": "target-invocation"})
    await asyncio.gather(task, return_exceptions=True)

    assert response["cancelled"] is True
    assert task.cancelled()


async def test_cancel_handler_waits_for_task_cleanup(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cleanup_ran = False

    async def _target() -> None:
        nonlocal cleanup_ran
        try:
            await asyncio.sleep(60)
        finally:
            cleanup_ran = True

    task = asyncio.create_task(_target())
    registry = InFlightInvocationRegistry(ttl_seconds=60, reaper_interval_s=60)
    registry.register(
        "cleanup-invocation",
        task,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=True,
    )
    monkeypatch.setattr(
        "sandbox.daemon.builtin_operations.get_in_flight_registry",
        lambda: registry,
    )

    await asyncio.sleep(0)
    response = await cancel_handler.cancel({"invocation_id": "cleanup-invocation"})

    assert response["cancelled"] is True
    assert response["cleanup_done"] is True
    assert cleanup_ran is True
    assert task.cancelled()


async def test_inflight_count_ignores_foreground_maintenance_invocation() -> None:
    foreground = asyncio.create_task(asyncio.sleep(60))
    background = asyncio.create_task(asyncio.sleep(60))
    registry = InFlightInvocationRegistry(ttl_seconds=60, reaper_interval_s=60)
    registry.register(
        "foreground-maintenance",
        foreground,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=False,
    )
    registry.register(
        "background-exec-command",
        background,
        agent_id="agent-a",
        op="api.v1.exec_command",
        background=True,
    )

    assert registry.count_by_agent("agent-a") == 1

    foreground.cancel()
    background.cancel()
    await asyncio.gather(foreground, background, return_exceptions=True)

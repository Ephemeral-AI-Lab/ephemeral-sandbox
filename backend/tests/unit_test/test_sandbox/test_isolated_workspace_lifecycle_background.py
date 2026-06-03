"""Engine-layer isolated-workspace background lifecycle checks."""

from __future__ import annotations

import importlib
from types import SimpleNamespace

import pytest

from sandbox._shared.models import (
    EnterIsolatedWorkspaceRequest,
    ExitIsolatedWorkspaceRequest,
    SandboxCaller,
)
from sandbox.host.isolated_workspace_lifecycle import (
    enter_isolated_workspace,
    exit_isolated_workspace,
)


pytestmark = pytest.mark.asyncio

enter_module = importlib.import_module("sandbox.host.isolated_workspace_lifecycle")
exit_module = enter_module  # consolidated; both live in sandbox.host.isolated_workspace_lifecycle


class _BackgroundManager:
    def __init__(self, count: int = 0) -> None:
        self.count = count
        self.cancelled: list[tuple[str, float]] = []

    def count_by_agent(self, agent_id: str) -> int:
        assert agent_id == "agent-a"
        return self.count

    async def cancel_by_agent(self, agent_id: str, *, grace_s: float) -> int:
        self.cancelled.append((agent_id, grace_s))
        return 2


async def test_enter_rejects_before_pipeline_when_background_tasks_exist(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    called = False

    async def ensure_pipeline(args: dict[str, object]) -> object:
        nonlocal called
        called = True
        return object()

    monkeypatch.setattr(
        enter_module.isolated_pipeline_registry,
        "ensure_pipeline",
        ensure_pipeline,
    )

    result = await enter_isolated_workspace(
        EnterIsolatedWorkspaceRequest(
            caller=SandboxCaller(agent_id="agent-a"),
            layer_stack_root="/tmp/stack",
        ),
        background_manager=_BackgroundManager(count=1),
    )

    assert result.success is False
    assert result.error is not None
    assert result.error.kind == "ephemeral_jobs_in_flight"
    assert result.error.details == {"count": "1"}
    assert called is False


async def test_exit_drains_background_tasks_before_pipeline_exit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []
    bg = _BackgroundManager()

    async def _cancel_by_agent(agent_id: str, *, grace_s: float) -> int:
        order.append(f"cancel:{agent_id}:{grace_s}")
        return 2

    async def _exit(agent_id: str, *, grace_s: float) -> dict[str, object]:
        order.append(f"exit:{agent_id}:{grace_s}")
        return {
            "success": True,
            "evicted_upperdir_bytes": 0,
            "lifetime_s": 1.0,
            "phases_ms": {"teardown": 1.0},
        }

    bg.cancel_by_agent = _cancel_by_agent  # type: ignore[method-assign]
    monkeypatch.setattr(
        exit_module.isolated_pipeline_registry,
        "require_pipeline",
        lambda: SimpleNamespace(exit=_exit),
    )

    result = await exit_isolated_workspace(
        ExitIsolatedWorkspaceRequest(
            caller=SandboxCaller(agent_id="agent-a"),
            grace_s=3.5,
        ),
        background_manager=bg,
    )

    assert result.success is True
    assert order == ["cancel:agent-a:3.5", "exit:agent-a:0.0"]
    assert result.phases_ms["evicted_background_tasks"] == 2.0


async def test_exit_drains_agent_background_tasks(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []
    bg = _BackgroundManager()

    async def _cancel_by_agent(agent_id: str, *, grace_s: float) -> int:
        order.append(f"cancel:{agent_id}:{grace_s}")
        return 1

    async def _exit(agent_id: str, *, grace_s: float) -> dict[str, object]:
        order.append(f"exit:{agent_id}:{grace_s}")
        return {
            "success": True,
            "evicted_upperdir_bytes": 0,
            "lifetime_s": 1.0,
            "phases_ms": {},
        }

    bg.cancel_by_agent = _cancel_by_agent  # type: ignore[method-assign]
    monkeypatch.setattr(
        exit_module.isolated_pipeline_registry,
        "require_pipeline",
        lambda: SimpleNamespace(exit=_exit),
    )

    result = await exit_isolated_workspace(
        ExitIsolatedWorkspaceRequest(
            caller=SandboxCaller(agent_id="agent-a"),
            grace_s=2.0,
        ),
        background_manager=bg,
    )

    assert result.success is True
    assert order == ["cancel:agent-a:2.0", "exit:agent-a:0.0"]
    assert result.phases_ms["evicted_background_tasks"] == 1.0


async def test_enter_fails_closed_when_daemon_command_session_count_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def _command_session_count(sandbox_id: str, agent_id: str) -> int:
        raise RuntimeError(f"{sandbox_id}:{agent_id}")

    monkeypatch.setattr("sandbox.api.command_session_count", _command_session_count)

    result = await enter_isolated_workspace(
        EnterIsolatedWorkspaceRequest(
            caller=SandboxCaller(agent_id="agent-a"),
            layer_stack_root="/tmp/stack",
        ),
        background_manager=_BackgroundManager(count=0),
        sandbox_id="sandbox-1",
    )

    assert result.success is False
    assert result.error is not None
    assert result.error.kind == "command_session_count_unavailable"
    assert result.error.details == {"sandbox_id": "sandbox-1"}

"""Tests for tools.sandbox.write_stdin."""

from __future__ import annotations

import importlib
from pathlib import Path
from typing import Any

import pytest

from engine.background.task_supervisor import BackgroundTaskSupervisor
from sandbox._shared.models import CommandOutput, ExecCommandResult
from tools._framework.core.base import ToolExecutionContextService
from tools.sandbox.write_stdin import write_stdin

from ._helpers import run_tool_safely

write_stdin_module = importlib.import_module("tools.sandbox.write_stdin.write_stdin")


class _WriteStdinApi:
    def __init__(self) -> None:
        self.write_calls: list[tuple[str, Any]] = []
        self.cancel_calls: list[tuple[str, Any]] = []

    async def write_stdin(self, sandbox_id: str, request: Any) -> ExecCommandResult:
        self.write_calls.append((sandbox_id, request))
        return ExecCommandResult(
            success=True,
            status="running",
            exit_code=None,
            output=CommandOutput(),
            command_session_id=request.command_session_id,
        )

    async def cancel_command_session(
        self,
        sandbox_id: str,
        request: Any,
    ) -> ExecCommandResult:
        self.cancel_calls.append((sandbox_id, request))
        return ExecCommandResult(
            success=False,
            status="cancelled",
            exit_code=None,
            output=CommandOutput(stderr="cancelled\n"),
            command_session_id=request.command_session_id,
        )


async def test_ctrl_c_cancels_running_command_session(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    api = _WriteStdinApi()
    supervisor = BackgroundTaskSupervisor()
    supervisor.register_command_session(
        command_session_id="cmd-1",
        sandbox_id="sb-1",
        agent_id="agent-1",
        command="python slow.py",
    )
    ctx = ToolExecutionContextService(
        cwd=Path("/tmp"),
        services={
            "sandbox_id": "sb-1",
            "agent_run_id": "agent-1",
            "background_task_manager": supervisor,
        },
    )
    monkeypatch.setattr(write_stdin_module, "sandbox_api", api)

    result = await run_tool_safely(
        write_stdin,
        {"command_session_id": "cmd-1", "chars": "\x03"},
        context=ctx,
    )

    assert result.metadata["status"] == "cancelled"
    assert len(api.write_calls) == 1
    assert len(api.cancel_calls) == 1
    assert supervisor.count_by_agent("agent-1") == 0

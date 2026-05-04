"""Tests for tools.sandbox_toolkit.shell."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest

from sandbox.api.utils.models import ShellResult
from tools.core.base import ToolExecutionContextService
from tools.core.safe_execution import run_tool_safely
import tools.sandbox_toolkit.shell as shell_module
from tools.sandbox_toolkit.shell import shell


class _ShellApi:
    def __init__(self, result: ShellResult) -> None:
        self.result = result
        self.calls: list[tuple[str, Any]] = []

    async def shell(self, sandbox_id: str, request: Any) -> ShellResult:
        self.calls.append((sandbox_id, request))
        return self.result


def _ctx_with_api(api: _ShellApi) -> ToolExecutionContextService:
    return ToolExecutionContextService(
        cwd=Path("/tmp"),
        services={
            "sandbox_id": "sb-1",
            "sandbox_api": api,
            "repo_root": "/ws",
        },
    )


def _run(args: dict[str, Any], ctx: ToolExecutionContextService):
    return asyncio.run(run_tool_safely(shell, args, context=ctx))


def test_shell_success_returns_single_command_output_shape(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    api = _ShellApi(
        ShellResult(
            exit_code=0,
            stdout="ok\n",
            success=True,
            changed_paths=("/ws/a.py",),
        )
    )
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(shell_module, "sandbox_shell", api.shell)

    result = _run({"command": "pytest -q"}, ctx)

    assert not result.is_error
    payload = json.loads(result.output)
    assert payload == {
        "cwd": "/ws",
        "status": "ok",
        "changed_paths": ["/ws/a.py"],
        "conflict_reason": None,
        "command": "pytest -q",
        "exit_code": 0,
        "stdout": "ok\n",
        "stderr": "",
        "error": "",
    }


def test_shell_conflict_returns_conflict_reason_without_legacy_fields(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    api = _ShellApi(
        ShellResult(
            exit_code=0,
            stdout="",
            success=False,
            changed_paths=("/ws/a.py",),
            conflict_reason="overlay upperdir is full",
        )
    )
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(shell_module, "sandbox_shell", api.shell)

    result = _run({"command": "python script.py"}, ctx)

    assert result.is_error
    payload = json.loads(result.output)
    assert payload == {
        "cwd": "/ws",
        "status": "error",
        "changed_paths": ["/ws/a.py"],
        "conflict_reason": "overlay upperdir is full",
        "command": "python script.py",
        "exit_code": 0,
        "stdout": "",
        "stderr": "",
        "error": "sandbox commit aborted: overlay upperdir is full",
    }

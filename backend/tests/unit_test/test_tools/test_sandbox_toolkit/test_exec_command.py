"""Tests for tools.sandbox.exec_command."""

from __future__ import annotations

import asyncio
import importlib
import json
from pathlib import Path
from typing import Any

import pytest

from sandbox._shared.models import CommandOutput, ExecCommandResult
from sandbox._shared.timing_keys import TimingKey
from tools._framework.core.base import ToolExecutionContextService
from tools.sandbox.exec_command import exec_command

from ._helpers import run_tool_safely

exec_command_module = importlib.import_module("tools.sandbox.exec_command.exec_command")


class _CommandApi:
    def __init__(self, result: ExecCommandResult) -> None:
        self.result = result
        self.calls: list[tuple[str, Any]] = []

    async def exec_command(
        self,
        sandbox_id: str,
        request: Any,
        **kwargs: Any,
    ) -> ExecCommandResult:
        self.calls.append((sandbox_id, request))
        self.kwargs = kwargs
        return self.result


def _ctx_with_api(api: _CommandApi) -> ToolExecutionContextService:
    return ToolExecutionContextService(
        cwd=Path("/tmp"),
        services={
            "sandbox_id": "sb-1",
            "sandbox_api": api,
            "repo_root": "/ws",
        },
    )


def _run(args: dict[str, Any], ctx: ToolExecutionContextService):
    return asyncio.run(run_tool_safely(exec_command, args, context=ctx))


def test_exec_command_success_returns_command_output_shape(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    api = _CommandApi(
        ExecCommandResult(
            status="ok",
            exit_code=0,
            output=CommandOutput(stdout="ok\n"),
            success=True,
            changed_paths=("/ws/a.py",),
            changed_path_kinds={"/ws/a.py": "write"},
            mutation_source="shell",
        )
    )
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(exec_command_module, "sandbox_api", api)

    result = _run({"cmd": "pytest -q"}, ctx)

    assert not result.is_error
    payload = json.loads(result.output)
    assert payload == {
        "status": "ok",
        "exit_code": 0,
        "output": {"stdout": "ok\n", "stderr": ""},
        "stdout": "ok\n",
        "stderr": "",
        "changed_paths": ["/ws/a.py"],
        "changed_path_kinds": {"/ws/a.py": "write"},
        "mutation_source": "shell",
        "conflict_reason": None,
    }


def test_exec_command_metadata_preserves_timing_key_enum_names(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    api = _CommandApi(
        ExecCommandResult(
            status="ok",
            exit_code=0,
            output=CommandOutput(stdout="ok\n"),
            success=True,
            timings={TimingKey.PREPARE_TOTAL: 0.1, TimingKey.APPLY_TOTAL: 0.2},
        )
    )
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(exec_command_module, "sandbox_api", api)

    result = _run({"cmd": "pytest -q"}, ctx)

    assert result.metadata["status"] == "ok"


def test_exec_command_conflict_returns_conflict_reason(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    api = _CommandApi(
        ExecCommandResult(
            status="error",
            exit_code=0,
            output=CommandOutput(),
            success=False,
            changed_paths=("/ws/a.py",),
            conflict_reason="overlay upperdir is full",
        )
    )
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(exec_command_module, "sandbox_api", api)

    result = _run({"cmd": "python script.py"}, ctx)

    assert result.is_error
    payload = json.loads(result.output)
    assert payload["status"] == "error"
    assert payload["changed_paths"] == ["/ws/a.py"]
    assert payload["conflict_reason"] == "overlay upperdir is full"


def test_exec_command_uses_publishable_audit_sink(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Sink:
        def publish(self, _event: object) -> None:
            return None

    api = _CommandApi(
        ExecCommandResult(status="ok", exit_code=0, output=CommandOutput(stdout="ok\n"))
    )
    sink = Sink()
    ctx = ToolExecutionContextService(
        cwd=Path("/tmp"),
        services={
            "sandbox_id": "sb-1",
            "repo_root": "/ws",
            "sandbox_audit_sink": sink,
        },
    )
    monkeypatch.setattr(exec_command_module, "sandbox_api", api)

    result = _run({"cmd": "pytest -q"}, ctx)

    assert api.kwargs == {"audit_sink": sink}
    assert result.metadata["sandbox_audit_emitted"] is True

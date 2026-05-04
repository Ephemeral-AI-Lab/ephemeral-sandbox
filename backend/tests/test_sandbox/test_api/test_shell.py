"""Tests for ``sandbox.api.shell``."""

from __future__ import annotations

import json
import shlex

from sandbox.api.utils.models import RawExecResult, RequestActor, ShellRequest
from sandbox.api.shell import shell
from sandbox.providers.registry import dispose_adapter, register_adapter


class _Adapter:
    name = "shell-api"

    def __init__(self, *, response: dict, expected_command: str = "pytest -q") -> None:
        self.response = response
        self.expected_command = expected_command
        self.calls: list[tuple[str, str, str | None, int | None]] = []

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        self.calls.append((sandbox_id, command, cwd, timeout))
        payload = json.loads(shlex.split(command)[-1])
        assert payload["op"] == "shell"
        assert payload["args"]["command"] == self.expected_command
        return RawExecResult(exit_code=0, stdout=json.dumps(self.response))


class _RawAdapter:
    name = "raw-shell-api"

    def __init__(self, *, stdout: str = "", exit_code: int = 0) -> None:
        self.stdout = stdout
        self.exit_code = exit_code
        self.calls: list[tuple[str, str, str | None, int | None]] = []

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        self.calls.append((sandbox_id, command, cwd, timeout))
        return RawExecResult(
            success=self.exit_code == 0,
            exit_code=self.exit_code,
            stdout=self.stdout,
        )


async def test_shell_delegates_once_and_round_trips_changed_paths() -> None:
    adapter = _Adapter(
        response={
            "result": "ok\n",
            "exit_code": 0,
            "changed_paths": ["/workspace/a.py"],
            "warnings": [],
            "overlay_run_timings": {},
            "overlay_stage_timings": {},
            "conflict": None,
        }
    )
    register_adapter("sb-shell", adapter)
    try:
        result = await shell(
            "sb-shell",
            ShellRequest(
                command="pytest -q",
                cwd="/workspace",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-shell")

    assert result.success is True
    assert result.stdout == "ok\n"
    assert result.changed_paths == ("/workspace/a.py",)
    assert result.conflict is None
    assert len(adapter.calls) == 1


async def test_shell_overlay_or_occ_failure_maps_conflict_info() -> None:
    adapter = _Adapter(
        response={
            "result": "",
            "exit_code": 0,
            "changed_paths": ["/workspace/a.py"],
            "warnings": [],
            "overlay_run_timings": {},
            "overlay_stage_timings": {},
            "conflict": {
                "reason": "overlay_upper_full",
                "conflict_file": "/workspace/a.py",
                "message": "upperdir full",
            },
        }
    )
    register_adapter("sb-shell-conflict", adapter)
    try:
        result = await shell(
            "sb-shell-conflict",
            ShellRequest(
                command="pytest -q",
                cwd="/workspace",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-shell-conflict")

    assert result.success is False
    assert result.status == "error"
    assert result.conflict is not None
    assert result.conflict.reason == "overlay_upper_full"
    assert result.conflict.conflict_file == "/workspace/a.py"
    assert result.conflict.message == "upperdir full"
    assert result.conflict_reason == "upperdir full"


async def test_shell_routes_read_only_pipeline_to_raw_exec() -> None:
    adapter = _RawAdapter(stdout="2\n")
    register_adapter("sb-shell-readonly", adapter)
    try:
        result = await shell(
            "sb-shell-readonly",
            ShellRequest(
                command="cat pyproject.toml | grep pytest | wc -l",
                cwd="/workspace",
                timeout=12,
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-shell-readonly")

    assert result.success is True
    assert result.status == "ok"
    assert result.exit_code == 0
    assert result.stdout == "2\n"
    assert result.changed_paths == ()
    assert adapter.calls == [
        ("sb-shell-readonly", "cat pyproject.toml | grep pytest | wc -l", "/workspace", 12)
    ]


async def test_shell_keeps_mutating_pipeline_on_overlay_route() -> None:
    command = "cat pyproject.toml | tee copied.txt"
    adapter = _Adapter(
        expected_command=command,
        response={
            "result": "ok\n",
            "exit_code": 0,
            "changed_paths": ["/workspace/copied.txt"],
            "warnings": [],
            "overlay_run_timings": {},
            "overlay_stage_timings": {},
            "conflict": None,
        },
    )
    register_adapter("sb-shell-mutating-pipeline", adapter)
    try:
        result = await shell(
            "sb-shell-mutating-pipeline",
            ShellRequest(
                command=command,
                cwd="/workspace",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-shell-mutating-pipeline")

    assert result.success is True
    assert result.changed_paths == ("/workspace/copied.txt",)
    assert len(adapter.calls) == 1


async def test_shell_keeps_control_operator_pipeline_on_overlay_route() -> None:
    command = "git status && rm -rf /tmp/project"
    adapter = _Adapter(
        expected_command=command,
        response={
            "result": "blocked by overlay path\n",
            "exit_code": 0,
            "changed_paths": [],
            "warnings": [],
            "overlay_run_timings": {},
            "overlay_stage_timings": {},
            "conflict": None,
        },
    )
    register_adapter("sb-shell-control", adapter)
    try:
        result = await shell(
            "sb-shell-control",
            ShellRequest(
                command=command,
                cwd="/workspace",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-shell-control")

    assert result.stdout == "blocked by overlay path\n"
    assert len(adapter.calls) == 1


async def test_shell_keeps_command_substitution_on_overlay_route() -> None:
    command = 'grep "$(rm -rf /tmp/project)" pyproject.toml | wc -l'
    adapter = _Adapter(
        expected_command=command,
        response={
            "result": "0\n",
            "exit_code": 0,
            "changed_paths": [],
            "warnings": [],
            "overlay_run_timings": {},
            "overlay_stage_timings": {},
            "conflict": None,
        },
    )
    register_adapter("sb-shell-substitution", adapter)
    try:
        result = await shell(
            "sb-shell-substitution",
            ShellRequest(
                command=command,
                cwd="/workspace",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-shell-substitution")

    assert result.stdout == "0\n"
    assert len(adapter.calls) == 1

"""Tests for ``sandbox.api.tool.shell``."""

from __future__ import annotations

import pytest

from sandbox.api import SandboxCaller, ShellRequest
from sandbox.api.tool.shell import shell


@pytest.mark.asyncio
async def test_shell_dispatches_to_sandbox_daemon(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "exit_code": 0,
            "stdout": "new\n",
            "stderr": "",
            "changed_paths": ["pkg/value.txt"],
            "status": "ok",
            "conflict": None,
            "conflict_reason": None,
            "warnings": [],
            "timings": {"api.shell.total_s": 0.2},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await shell(
        "sb-shell",
        ShellRequest(
            command="printf 'new\\n'",
            cwd=".",
            timeout=12,
            caller=SandboxCaller(agent_id="agent-1"),
            description="shell test",
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.status == "ok"
    assert result.exit_code == 0
    assert result.stdout == "new\n"
    assert result.changed_paths == ("pkg/value.txt",)
    assert transport.calls == [
        (
            "sb-shell",
            "api.v1.shell",
            {
                "command": "printf 'new\\n'",
                "cwd": ".",
                "timeout_seconds": 12,
                "actor_id": "agent-1",
                "caller": {
                    "agent_id": "agent-1",
                    "run_id": "",
                    "agent_run_id": "",
                    "task_id": "",
                },
                "description": "shell test",
            },
            42,
        )
    ]


@pytest.mark.asyncio
async def test_shell_overlay_policy_error_maps_to_rejected_result(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(_sandbox_id, _op, _args, _timeout):
        raise RuntimeError(
            "internal_error: overlay capture refuses escaping symlink target: "
            ".ephemeralos/sweevo-mock/full_stack/overlay/symlink_escape"
        )

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await shell(
        "sb-shell",
        ShellRequest(
            command="ln -s /tmp/outside link",
            cwd=".",
            timeout=12,
            caller=SandboxCaller(agent_id="agent-1"),
            description="shell test",
        ),
        transport=transport,
    )

    assert result.success is False
    assert result.status == "rejected"
    assert result.conflict is not None
    assert result.conflict.reason == "rejected"
    assert result.conflict_reason is not None
    assert "internal_error" not in result.conflict_reason
    assert "overlay capture refuses escaping symlink target" in result.conflict_reason


@pytest.mark.asyncio
async def test_shell_rejects_stdin_without_daemon_dispatch(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fail_call_daemon_api(_sandbox_id, _op, _args, _timeout):
        raise AssertionError("daemon dispatch should not be called")

    del monkeypatch
    transport = recording_transport_factory(fail_call_daemon_api)

    result = await shell(
        "sb-shell",
        ShellRequest(
            command="cat",
            stdin="input",
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is False
    assert result.status == "error"
    assert result.conflict is not None
    assert result.conflict.reason == "stdin_not_supported"
    assert transport.calls == []

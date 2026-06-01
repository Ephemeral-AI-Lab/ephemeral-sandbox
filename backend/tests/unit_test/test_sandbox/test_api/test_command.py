"""Tests for Phase 3T command API wrappers."""

from __future__ import annotations

import pytest

from sandbox.api import (
    ExecCommandRequest,
    PtyCancelRequest,
    PtyProgressRequest,
    PtyWriteRequest,
    SandboxCaller,
)
from sandbox.api.tool.command import (
    cancel_pty_command,
    check_pty_command_progress,
    exec_command,
    write_pty_command_stdin,
)


@pytest.mark.asyncio
async def test_exec_command_dispatches_final_wire_shape(recording_transport_factory) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "status": "running",
            "exit_code": None,
            "output": {"stdout": "ready\n", "stderr": ""},
            "pty_session_id": "pty_1",
            "timings": {"api.exec_command.total_s": 0.01},
        }

    transport = recording_transport_factory(fake_call_daemon_api)

    result = await exec_command(
        "sb-command",
        ExecCommandRequest(
            invocation_id="inv-command",
            cmd="python -i",
            tty=True,
            yield_time_ms=50,
            timeout=12,
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.status == "running"
    assert result.pty_session_id == "pty_1"
    assert result.output.stdout == "ready\n"
    assert transport.calls == [
        (
            "sb-command",
            "api.v1.exec_command",
            {
                "cmd": "python -i",
                "tty": True,
                "yield_time_ms": 50,
                "timeout": 12,
                "invocation_id": "inv-command",
                "agent_id": "agent-1",
                "caller": {
                    "agent_id": "agent-1",
                    "run_id": "",
                    "agent_run_id": "",
                    "task_id": "",
                },
            },
            42,
        )
    ]


@pytest.mark.asyncio
async def test_pty_control_wrappers_parse_generic_not_found(recording_transport_factory) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "status": "error",
            "exit_code": None,
            "output": {"stdout": "", "stderr": "pty_session_not_found"},
        }

    transport = recording_transport_factory(fake_call_daemon_api)
    caller = SandboxCaller(agent_id="agent-1")

    write = await write_pty_command_stdin(
        "sb-command",
        PtyWriteRequest(caller=caller, pty_session_id="pty_missing", chars="x"),
        transport=transport,
    )
    progress = await check_pty_command_progress(
        "sb-command",
        PtyProgressRequest(caller=caller, pty_session_id="pty_missing", time=1),
        transport=transport,
    )
    cancel = await cancel_pty_command(
        "sb-command",
        PtyCancelRequest(caller=caller, pty_session_id="pty_missing"),
        transport=transport,
    )

    assert write.output.stderr == "pty_session_not_found"
    assert progress.output.stderr == "pty_session_not_found"
    assert cancel.output.stderr == "pty_session_not_found"
    assert [call[1] for call in transport.calls] == [
        "api.v1.pty.write_stdin",
        "api.v1.pty.progress",
        "api.v1.pty.cancel",
    ]

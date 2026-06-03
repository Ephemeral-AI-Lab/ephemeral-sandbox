"""Tests for shared sandbox command result rendering."""

from __future__ import annotations

import asyncio
import json

import pytest

from engine.background.task_supervisor import BackgroundTaskSupervisor
from sandbox._shared.models import CommandOutput, ExecCommandResult
from tools._framework.core.context import ToolExecutionContextService
from tools.sandbox._lib.command_session_tool import (
    command_tool_result,
    recover_command_session_result_from_supervisor,
)


def test_command_tool_result_marks_timeout_as_error() -> None:
    result = command_tool_result(
        ExecCommandResult(
            success=False,
            status="timed_out",
            exit_code=124,
            output=CommandOutput(stderr="timeout\n"),
            command_session_id="cmd_1",
        )
    )

    assert result.is_error is True
    assert result.metadata == {
        "status": "timed_out",
        "command_session_id": "cmd_1",
        "changed_paths": [],
        "changed_path_kinds": {},
        "mutation_source": "",
        "conflict_reason": None,
    }
    assert json.loads(result.output) == {
        "status": "timed_out",
        "exit_code": 124,
        "output": {"stdout": "", "stderr": "timeout\n"},
        "stdout": "",
        "stderr": "timeout\n",
        "changed_paths": [],
        "changed_path_kinds": {},
        "mutation_source": "",
        "conflict_reason": None,
        "command_session_id": "cmd_1",
    }


def test_command_tool_result_preserves_timings_metadata() -> None:
    result = command_tool_result(
        ExecCommandResult(
            success=True,
            status="ok",
            exit_code=0,
            output=CommandOutput(stdout="ok\n"),
            timings={
                "command_exec.total_s": 0.25,
                "api.exec_command.dispatch_total_s": 0.3,
            },
        )
    )

    assert result.metadata["timings"] == {
        "command_exec.total_s": 0.25,
        "api.exec_command.dispatch_total_s": 0.3,
    }


@pytest.mark.asyncio
async def test_command_session_not_found_recovers_supervisor_terminal_result() -> None:
    supervisor = BackgroundTaskSupervisor()
    supervisor.register_command_session(
        command_session_id="cmd_2",
        sandbox_id="sb-1",
        agent_id="agent-1",
        command="printf done",
    )
    supervisor.mark_command_session_result_reported_by_tool(
        command_session_id="cmd_2",
        result={
            "status": "ok",
            "exit_code": 0,
            "output": {"stdout": "done\n", "stderr": ""},
        },
    )
    missing = ExecCommandResult(
        success=False,
        status="error",
        exit_code=None,
        output=CommandOutput(stderr="command_session_not_found"),
    )

    recovered = recover_command_session_result_from_supervisor(
        ToolExecutionContextService(
            cwd=".",
            services={"background_task_manager": supervisor},
        ),
        missing,
        command_session_id="cmd_2",
    )

    assert recovered.status == "ok"
    assert recovered.exit_code == 0
    assert recovered.command_session_id == "cmd_2"
    assert recovered.output.stdout == "done\n"
    await asyncio.sleep(0)

"""Tests for shared CI runtime helpers."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContext
from tools.core.ci_runtime import (
    exec_ci_process_operation,
    get_ci_service,
)
from tools.daytona_toolkit.ci_integration import destructive_shell_command_error


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


# ---------------------------------------------------------------------------
# get_ci_service
# ---------------------------------------------------------------------------


def test_get_ci_service_returns_none_when_missing():
    ctx = _ctx()
    assert get_ci_service(ctx) is None


def test_get_ci_service_returns_value():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    assert get_ci_service(ctx) is svc


@pytest.mark.asyncio
async def test_exec_ci_process_operation_delegates_audited_process_call():
    sandbox = MagicMock()
    svc = MagicMock()
    svc.exec_process_operation = AsyncMock(return_value=SimpleNamespace(result="ok", exit_code=0))
    ctx = _ctx(
        {
            "ci_service": svc,
            "agent_name": "developer",
            "team_run_id": "team-1",
            "agent_run_id": "agent-1",
            "work_item_id": "task-1",
        }
    )

    result = await exec_ci_process_operation(
        ctx,
        sandbox,
        "echo ok",
        timeout=12,
        description="daytona_codeact shell",
        edit_type="codeact",
    )

    assert result.result == "ok"
    svc.exec_process_operation.assert_awaited_once_with(
        sandbox,
        "echo ok",
        timeout=12,
        description="daytona_codeact shell",
        edit_type="codeact",
        agent_id="developer",
        team_run_id="team-1",
        agent_run_id="agent-1",
        task_id="task-1",
    )


# ---------------------------------------------------------------------------
# destructive_shell_command_error — shell policy regression tests
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "command",
    [
        "rm -rf /testbed/dask",
        "rm -rF /testbed",
        "rm --recursive /workspace/project",
        "mv /testbed/dask /tmp/trash",
        "mv /home/user /tmp",
        "mkfs.ext4 /dev/sda1",
        "dd if=/dev/zero of=/dev/sda",
        "rm -rf .",
        "echo ok; rm -rf /testbed/dask",
    ],
)
def test_destructive_shell_command_error_blocks(command):
    err = destructive_shell_command_error(command)
    assert err is not None, f"Should block: {command}"
    assert "BLOCKED" in err


@pytest.mark.parametrize(
    "command",
    [
        "rm /testbed/dask/file.py",
        "rm -f /testbed/dask/file.py",
        "mv /testbed/dask/file.py /testbed/dask/new.py",
        "cp -r /testbed/dask /testbed/backup",
        "pytest /testbed/dask/tests",
        "python -c 'import os'",
        "",
    ],
)
def test_destructive_shell_command_error_allows_safe(command):
    err = destructive_shell_command_error(command)
    assert err is None, f"Should allow: {command}"

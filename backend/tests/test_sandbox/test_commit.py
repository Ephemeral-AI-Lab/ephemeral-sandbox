"""Tests for the sandbox OCC commit façade."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from sandbox.occ.types import OperationResult
from sandbox.lifecycle.commit import (
    FileChangeResult,
    submit_commit,
    submit_shell_cmd,
)

pytestmark = pytest.mark.asyncio


def _op_result(
    *,
    success: bool = True,
    status: str = "committed",
    paths: list[str] | None = None,
    conflict_reason: str = "",
) -> OperationResult:
    files = tuple(SimpleNamespace(file_path=p) for p in (paths or []))
    return OperationResult(
        success=success,
        status=status,  # type: ignore[arg-type]
        files=files,  # type: ignore[arg-type]
        conflict_file=None,
        conflict_reason=conflict_reason,
        timings={},
    )


def _svc_with_op(method_name: str, result: OperationResult) -> MagicMock:
    svc = MagicMock()
    svc.rebind_sandbox = MagicMock()
    setattr(svc, method_name, MagicMock(return_value=result))
    return svc


async def test_submit_commit_success_exposes_changed_paths() -> None:
    svc = _svc_with_op("write_file", _op_result(paths=["/ws/a.py", "/ws/b.py"]))

    change = await submit_commit(
        svc,
        op="write",
        specs=[],
        fallback_paths=["/ws/a.py"],
        description="write",
        agent_id="test-agent",
    )

    assert isinstance(change, FileChangeResult)
    assert change.success is True
    assert change.changed_paths == ("/ws/a.py", "/ws/b.py")
    assert change.conflict_reason is None
    assert change.raw is svc.write_file.return_value


async def test_submit_commit_failure_surfaces_conflict_reason() -> None:
    svc = _svc_with_op(
        "edit_file",
        _op_result(success=False, status="aborted_version", conflict_reason="version_drift"),
    )

    change = await submit_commit(
        svc,
        op="edit",
        specs=[],
        fallback_paths=["/ws/x.py"],
        description="edit",
        agent_id="test-agent",
    )

    assert change.success is False
    assert change.conflict_reason == "version_drift"
    assert change.changed_paths == ("/ws/x.py",)  # fallback when service reports no files


async def test_submit_commit_preserves_successful_empty_file_set() -> None:
    svc = _svc_with_op("delete_file", _op_result(paths=[]))

    change = await submit_commit(
        svc,
        op="delete",
        specs=["/ws/gone.py"],
        fallback_paths=["/ws/gone.py"],
        description="delete",
        agent_id="test-agent",
    )

    assert change.changed_paths == ()


async def test_submit_commit_rejects_when_ci_service_missing() -> None:
    with pytest.raises(RuntimeError, match="submit_commit requires"):
        await submit_commit(
            None,
            op="write",
            specs=[],
            fallback_paths=[],
            description="write",
            agent_id="test-agent",
        )


async def test_submit_shell_cmd_normalizes_changed_paths() -> None:
    response = SimpleNamespace(
        result="ok",
        exit_code=0,
        changed_paths=["/ws/b.py", "/ws/a.py", "/ws/a.py", "  "],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)

    change = await submit_shell_cmd(
        svc,
        object(),
        command="echo hi",
        description="test",
        agent_id="test-agent",
    )

    assert change.success is True
    # Sorted + deduped + empty-filtered.
    assert change.changed_paths == ("/ws/a.py", "/ws/b.py")
    assert change.raw is response


async def test_submit_shell_cmd_marks_nonzero_exit_as_failure() -> None:
    response = SimpleNamespace(
        result="",
        exit_code=1,
        changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)

    change = await submit_shell_cmd(
        svc,
        object(),
        command="false",
        description="test",
        agent_id="test-agent",
    )

    assert change.success is False


async def test_submit_shell_cmd_treats_noop_commit_status_as_success() -> None:
    response = SimpleNamespace(
        result="ok",
        exit_code=0,
        changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)

    change = await submit_shell_cmd(
        svc,
        object(),
        command="python3 -m venv .venv",
        description="test",
        agent_id="test-agent",
    )

    assert change.success is True
    assert change.changed_paths == ()


async def test_submit_shell_cmd_treats_sandbox_commit_abort_as_failure() -> None:
    response = SimpleNamespace(
        result="",
        exit_code=0,
        changed_paths=[],
        conflict_reason="version_drift",
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)

    change = await submit_shell_cmd(
        svc,
        object(),
        command="echo hi",
        description="test",
        agent_id="test-agent",
    )

    assert change.success is False
    assert change.conflict_reason == "version_drift"


async def test_submit_shell_cmd_rejects_when_no_sandbox_available() -> None:
    svc = MagicMock()
    svc.cmd = AsyncMock()

    with pytest.raises(RuntimeError, match="requires a sandbox"):
        await submit_shell_cmd(
            svc,
            None,
            command="echo hi",
            description="test",
            agent_id="test-agent",
        )


async def test_submit_shell_cmd_passes_explicit_sandbox_through() -> None:
    response = SimpleNamespace(
        result="ok", exit_code=0, changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    explicit_sandbox = object()

    change = await submit_shell_cmd(
        svc,
        explicit_sandbox,
        command="echo hi",
        description="test",
        agent_id="test-agent",
    )

    assert change.success is True
    called_sandbox = svc.cmd.await_args.args[0]
    assert called_sandbox is explicit_sandbox


async def test_submit_shell_cmd_forwards_progress_callback() -> None:
    response = SimpleNamespace(
        result="ok", exit_code=0, changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)

    def on_progress(line: str) -> None:
        del line

    change = await submit_shell_cmd(
        svc,
        object(),
        command="echo hi",
        description="test",
        agent_id="test-agent",
        on_progress_line=on_progress,
    )

    assert change.success is True
    assert svc.cmd.await_args.kwargs["on_progress_line"] is on_progress


async def test_submit_shell_cmd_forwards_attribution_kwargs() -> None:
    response = SimpleNamespace(
        result="ok", exit_code=0, changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)

    await submit_shell_cmd(
        svc,
        object(),
        command="echo hi",
        description="test",
        agent_id="agent-1",
        run_id="run-1",
        agent_run_id="agent-run-1",
        task_id="task-1",
    )

    kwargs = svc.cmd.await_args.kwargs
    assert kwargs["agent_id"] == "agent-1"
    assert kwargs["run_id"] == "run-1"
    assert kwargs["agent_run_id"] == "agent-run-1"
    assert kwargs["task_id"] == "task-1"

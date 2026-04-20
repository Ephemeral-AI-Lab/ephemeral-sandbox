"""Tests for the OCC commit façade in tools.daytona_toolkit._commit."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from code_intelligence.types import OperationResult
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._commit import (
    FileChangeResult,
    submit_codeact_cmd,
    submit_commit,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata: dict | None = None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/ws"), metadata=metadata or {})


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
    ctx = _ctx({"ci_service": svc})

    change = await submit_commit(
        ctx,
        op="write",
        specs=[],
        fallback_paths=["/ws/a.py"],
        description="write",
    )

    assert isinstance(change, FileChangeResult)
    assert change.success is True
    assert change.changed_paths == ("/ws/a.py", "/ws/b.py")
    assert change.ambient_changed_paths == ()
    assert change.conflict_reason is None
    assert change.raw is svc.write_file.return_value


async def test_submit_commit_failure_surfaces_conflict_reason() -> None:
    svc = _svc_with_op(
        "edit_file",
        _op_result(success=False, status="aborted_version", conflict_reason="version_drift"),
    )
    ctx = _ctx({"ci_service": svc})

    change = await submit_commit(
        ctx,
        op="edit",
        specs=[],
        fallback_paths=["/ws/x.py"],
        description="edit",
    )

    assert change.success is False
    assert change.conflict_reason == "version_drift"
    assert change.changed_paths == ("/ws/x.py",)  # fallback when service reports no files


async def test_submit_commit_preserves_successful_empty_file_set() -> None:
    svc = _svc_with_op("delete_file", _op_result(paths=[]))
    ctx = _ctx({"ci_service": svc})

    change = await submit_commit(
        ctx,
        op="delete",
        specs=["/ws/gone.py"],
        fallback_paths=["/ws/gone.py"],
        description="delete",
    )

    assert change.changed_paths == ()


async def test_submit_commit_rejects_when_ci_service_missing() -> None:
    ctx = _ctx({})

    with pytest.raises(RuntimeError, match="submit_commit requires"):
        await submit_commit(
            ctx,
            op="write",
            specs=[],
            fallback_paths=[],
            description="write",
        )


async def test_submit_commit_dispatches_on_op_name() -> None:
    # The façade should only touch the method it needs; mocks that don't
    # implement the other three siblings must still work.
    svc = MagicMock(spec=["rebind_sandbox", "move_file"])
    svc.rebind_sandbox = MagicMock()
    svc.move_file = MagicMock(return_value=_op_result(paths=["/ws/b"]))
    ctx = _ctx({"ci_service": svc})

    change = await submit_commit(
        ctx,
        op="move",
        specs=[],
        fallback_paths=["/ws/b"],
        description="move",
    )

    assert change.success is True
    assert change.changed_paths == ("/ws/b",)


async def test_submit_codeact_cmd_normalizes_changed_paths() -> None:
    response = SimpleNamespace(
        result="ok",
        exit_code=0,
        changed_paths=["/ws/b.py", "/ws/a.py", "/ws/a.py", "  "],
        ambient_changed_paths=["/ws/c.py"],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    ctx = _ctx({"ci_service": svc, "ci_sandbox": object()})

    change = await submit_codeact_cmd(
        ctx,
        command="echo hi",
        description="test",
    )

    assert change.success is True
    # Sorted + deduped + empty-filtered.
    assert change.changed_paths == ("/ws/a.py", "/ws/b.py")
    assert change.ambient_changed_paths == ("/ws/c.py",)
    assert change.raw is response


async def test_submit_codeact_cmd_marks_nonzero_exit_as_failure() -> None:
    response = SimpleNamespace(
        result="",
        exit_code=1,
        changed_paths=[],
        ambient_changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    ctx = _ctx({"ci_service": svc, "ci_sandbox": object()})

    change = await submit_codeact_cmd(
        ctx,
        command="false",
        description="test",
    )

    assert change.success is False


async def test_submit_codeact_cmd_treats_noop_commit_status_as_success() -> None:
    response = SimpleNamespace(
        result="ok",
        exit_code=0,
        changed_paths=[],
        ambient_changed_paths=[],
        git_commit_status="noop",
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    ctx = _ctx({"ci_service": svc, "ci_sandbox": object()})

    change = await submit_codeact_cmd(
        ctx,
        command="python3 -m venv .venv",
        description="test",
    )

    assert change.success is True
    assert change.changed_paths == ()


async def test_submit_codeact_cmd_treats_sandbox_commit_abort_as_failure() -> None:
    response = SimpleNamespace(
        result="",
        exit_code=0,
        changed_paths=[],
        ambient_changed_paths=[],
        git_commit_status="aborted_version",
        git_conflict_reason="version_drift",
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    ctx = _ctx({"ci_service": svc, "ci_sandbox": object()})

    change = await submit_codeact_cmd(
        ctx,
        command="echo hi",
        description="test",
    )

    assert change.success is False
    assert change.conflict_reason == "version_drift"


async def test_submit_codeact_cmd_rejects_when_no_sandbox_available() -> None:
    svc = MagicMock()
    svc.cmd = AsyncMock()
    ctx = _ctx({"ci_service": svc})

    with pytest.raises(RuntimeError, match="requires a sandbox"):
        await submit_codeact_cmd(
            ctx,
            command="echo hi",
            description="test",
        )


async def test_submit_codeact_cmd_uses_explicit_sandbox_override() -> None:
    response = SimpleNamespace(
        result="ok", exit_code=0, changed_paths=[], ambient_changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    recovered_sandbox = object()
    # No sandbox in context metadata — caller passes an explicit override.
    ctx = _ctx({"ci_service": svc})

    change = await submit_codeact_cmd(
        ctx,
        command="echo hi",
        description="test",
        sandbox=recovered_sandbox,
    )

    assert change.success is True
    called_sandbox = svc.cmd.await_args.args[0]
    assert called_sandbox is recovered_sandbox


async def test_submit_codeact_cmd_forwards_stdin() -> None:
    response = SimpleNamespace(
        result="ok", exit_code=0, changed_paths=[], ambient_changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    ctx = _ctx({"ci_service": svc, "ci_sandbox": object()})

    change = await submit_codeact_cmd(
        ctx,
        command="python3 -",
        description="test",
        stdin="print('hi')",
    )

    assert change.success is True
    assert svc.cmd.await_args.kwargs["stdin"] == "print('hi')"


async def test_submit_codeact_cmd_forwards_progress_callback() -> None:
    response = SimpleNamespace(
        result="ok", exit_code=0, changed_paths=[], ambient_changed_paths=[],
    )
    svc = MagicMock()
    svc.cmd = AsyncMock(return_value=response)
    ctx = _ctx({"ci_service": svc, "ci_sandbox": object()})

    def on_progress(line: str) -> None:
        del line

    change = await submit_codeact_cmd(
        ctx,
        command="echo hi",
        description="test",
        on_progress_line=on_progress,
    )

    assert change.success is True
    assert svc.cmd.await_args.kwargs["on_progress_line"] is on_progress

"""Tests for daytona_delete_file and daytona_move_file."""

from __future__ import annotations

import asyncio
import json
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from code_intelligence.types import EditResult, OperationResult
from tools.core.base import ToolExecutionContext, run_tool_safely
from tools.daytona_toolkit.delete_move_tool import (
    daytona_delete_file,
    daytona_move_file,
)


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=dict(metadata or {}))


def _run(tool, payload, ctx):
    return asyncio.run(run_tool_safely(tool, payload, ctx))


def _operation_result(
    *,
    success: bool,
    status: str = "committed",
    paths: list[str] | None = None,
    conflict_file: str | None = None,
    conflict_reason: str = "",
) -> OperationResult:
    return OperationResult(
        success=success,
        status=status,  # type: ignore[arg-type]
        files=tuple(
            EditResult(
                success=success,
                file_path=path,
                message=conflict_reason,
                conflict=not success,
                conflict_reason=status if status.startswith("aborted") else "",
            )
            for path in (paths or [])
        ),
        conflict_file=conflict_file,
        conflict_reason=conflict_reason,
        timings={},
    )


def _svc(
    *,
    delete_result: OperationResult | None = None,
    move_result: OperationResult | None = None,
):
    svc = MagicMock()
    svc.delete_file = MagicMock(
        return_value=delete_result
        or _operation_result(success=True, paths=["/ws/gone.py"])
    )
    svc.move_file = MagicMock(
        return_value=move_result
        or _operation_result(success=True, paths=["/ws/src.py", "/ws/dst.py"])
    )
    svc.rebind_sandbox = MagicMock()
    return svc


# ---------------------------------------------------------------------------
# daytona_delete_file
# ---------------------------------------------------------------------------


def test_delete_file_success_routes_through_occ_service() -> None:
    svc = _svc(delete_result=_operation_result(success=True, paths=["/ws/gone.py"]))
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws", "agent_run_id": "run-1"})

    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)

    payload = json.loads(result.output)
    assert result.is_error is False
    assert payload["status"] == "deleted"
    assert payload["paths"] == ["/ws/gone.py"]
    svc.delete_file.assert_called_once_with(
        "/ws/gone.py",
        agent_id="run-1",
        description="delete /ws/gone.py",
    )
    svc.exec_process_operation.assert_not_called()


def test_delete_file_rebinds_real_sandbox_before_occ_call() -> None:
    svc = _svc(delete_result=_operation_result(success=True, paths=["/ws/gone.py"]))
    sandbox = SimpleNamespace()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws", "daytona_sandbox": sandbox})

    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)

    assert result.is_error is False
    svc.rebind_sandbox.assert_called_once_with(sandbox)


def test_delete_file_rebinds_async_sandbox_to_sync_occ_handle() -> None:
    async def _async_exec(_command: str) -> object:
        raise AssertionError("async sandbox exec should not be used by sync OCC")

    svc = _svc(delete_result=_operation_result(success=True, paths=["/ws/gone.py"]))
    async_sandbox = SimpleNamespace(process=SimpleNamespace(exec=_async_exec))
    sync_sandbox = SimpleNamespace(process=SimpleNamespace(exec=MagicMock()))
    ctx = _ctx(
        {
            "ci_service": svc,
            "repo_root": "/ws",
            "sandbox_id": "sb-123",
            "daytona_sandbox": async_sandbox,
        }
    )

    with patch("sandbox.service.SandboxService") as service_cls:
        service_cls.return_value.get_sandbox_object.return_value = sync_sandbox
        result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)

    assert result.is_error is False
    svc.rebind_sandbox.assert_called_once_with(sync_sandbox)


def test_delete_file_occ_call_runs_off_active_event_loop_thread() -> None:
    caller_thread = threading.get_ident()

    class ThreadCheckingService:
        def __init__(self) -> None:
            self.rebind_sandbox = MagicMock()
            self.exec_process_operation = MagicMock()
            self.delete_file = MagicMock(side_effect=self._delete_file)

        def _delete_file(self, *args, **kwargs) -> OperationResult:
            assert threading.get_ident() != caller_thread
            return _operation_result(success=True, paths=["/ws/gone.py"])

    svc = ThreadCheckingService()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws", "agent_run_id": "run-1"})

    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)

    assert result.is_error is False
    svc.delete_file.assert_called_once_with(
        "/ws/gone.py",
        agent_id="run-1",
        description="delete /ws/gone.py",
    )
    svc.exec_process_operation.assert_not_called()


def test_delete_file_ci_required_when_service_missing() -> None:
    ctx = _ctx({"daytona_sandbox": SimpleNamespace(), "repo_root": "/ws"})
    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)
    assert result.is_error is True
    assert "ci_required" in (result.metadata or {})


def test_delete_file_reports_not_found() -> None:
    svc = _svc(
        delete_result=_operation_result(
            success=False,
            status="failed",
            paths=["/ws/missing.py"],
            conflict_file="/ws/missing.py",
            conflict_reason="not_found",
        )
    )
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = _run(daytona_delete_file, {"file_path": "/ws/missing.py"}, ctx)

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "not_found"
    assert payload["conflict_reason"] == "not_found"


def test_delete_file_reports_aborted_version_without_merge_fallback() -> None:
    svc = _svc(
        delete_result=_operation_result(
            success=False,
            status="aborted_version",
            paths=["/ws/gone.py"],
            conflict_file="/ws/gone.py",
            conflict_reason="file content changed before delete",
        )
    )
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "aborted_version"
    assert payload["conflict_reason"] == "file content changed before delete"


def test_delete_file_records_write_scope_warning() -> None:
    svc = _svc(delete_result=_operation_result(success=True, paths=["/ws/other/file.py"]))
    ctx = _ctx(
        {
            "ci_service": svc,
            "agent_name": "developer",
            "repo_root": "/ws",
            "write_scope": ["allowed/"],
        }
    )
    result = _run(daytona_delete_file, {"file_path": "/ws/other/file.py"}, ctx)
    assert result.is_error is False
    warnings = ctx.metadata.get("coordination_warnings") or []
    assert any(w.get("category") == "outside_write_scope" for w in warnings)


def test_delete_file_recursive_is_rejected_before_mutation() -> None:
    svc = _svc()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = _run(
        daytona_delete_file,
        {"file_path": "/ws/pkg", "recursive": True},
        ctx,
    )

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "failed"
    assert payload["conflict_reason"] == "recursive_unsupported"
    svc.delete_file.assert_not_called()


def test_delete_file_rejects_repo_root() -> None:
    svc = _svc()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = _run(daytona_delete_file, {"file_path": "/ws"}, ctx)

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "failed"
    assert "repo root" in payload["message"]
    svc.delete_file.assert_not_called()


# ---------------------------------------------------------------------------
# daytona_move_file
# ---------------------------------------------------------------------------


def test_move_file_success_routes_through_occ_service() -> None:
    svc = _svc(move_result=_operation_result(success=True, paths=["/ws/src.py", "/ws/dst.py"]))
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws", "agent_run_id": "run-2"})
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py"},
        ctx,
    )
    payload = json.loads(result.output)
    assert result.is_error is False
    assert payload["status"] == "moved"
    assert payload["paths"] == ["/ws/src.py", "/ws/dst.py"]
    svc.move_file.assert_called_once_with(
        "/ws/src.py",
        "/ws/dst.py",
        overwrite=False,
        agent_id="run-2",
        description="move /ws/src.py -> /ws/dst.py",
    )
    svc.exec_process_operation.assert_not_called()


def test_move_file_overwrite_passes_strict_base_intent_to_service() -> None:
    svc = _svc(move_result=_operation_result(success=True, paths=["/ws/a", "/ws/b"]))
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})
    _run(
        daytona_move_file,
        {"src_path": "/ws/a", "dst_path": "/ws/b", "overwrite": True},
        ctx,
    )
    assert svc.move_file.call_args.kwargs["overwrite"] is True


def test_move_file_occ_call_runs_off_active_event_loop_thread() -> None:
    caller_thread = threading.get_ident()

    class ThreadCheckingService:
        def __init__(self) -> None:
            self.rebind_sandbox = MagicMock()
            self.exec_process_operation = MagicMock()
            self.move_file = MagicMock(side_effect=self._move_file)

        def _move_file(self, *args, **kwargs) -> OperationResult:
            assert threading.get_ident() != caller_thread
            return _operation_result(success=True, paths=["/ws/src.py", "/ws/dst.py"])

    svc = ThreadCheckingService()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws", "agent_run_id": "run-2"})

    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py"},
        ctx,
    )

    assert result.is_error is False
    svc.move_file.assert_called_once_with(
        "/ws/src.py",
        "/ws/dst.py",
        overwrite=False,
        agent_id="run-2",
        description="move /ws/src.py -> /ws/dst.py",
    )
    svc.exec_process_operation.assert_not_called()


def test_move_file_rebinds_async_sandbox_to_sync_occ_handle() -> None:
    async def _async_exec(_command: str) -> object:
        raise AssertionError("async sandbox exec should not be used by sync OCC")

    svc = _svc(move_result=_operation_result(success=True, paths=["/ws/src.py", "/ws/dst.py"]))
    async_sandbox = SimpleNamespace(process=SimpleNamespace(exec=_async_exec))
    sync_sandbox = SimpleNamespace(process=SimpleNamespace(exec=MagicMock()))
    ctx = _ctx(
        {
            "ci_service": svc,
            "repo_root": "/ws",
            "sandbox_id": "sb-123",
            "daytona_sandbox": async_sandbox,
        }
    )

    with patch("sandbox.service.SandboxService") as service_cls:
        service_cls.return_value.get_sandbox_object.return_value = sync_sandbox
        result = _run(
            daytona_move_file,
            {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py"},
            ctx,
        )

    assert result.is_error is False
    svc.rebind_sandbox.assert_called_once_with(sync_sandbox)


def test_move_file_dst_exists_without_overwrite() -> None:
    svc = _svc(
        move_result=_operation_result(
            success=False,
            status="failed",
            paths=["/ws/dst.py"],
            conflict_file="/ws/dst.py",
            conflict_reason="dst_exists",
        )
    )
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py"},
        ctx,
    )
    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "dst_exists"


def test_move_file_overwrite_aborts_on_destination_drift() -> None:
    svc = _svc(
        move_result=_operation_result(
            success=False,
            status="aborted_version",
            paths=["/ws/src.py", "/ws/dst.py"],
            conflict_file="/ws/dst.py",
            conflict_reason="file content changed since base was captured (strict_base=True)",
        )
    )
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py", "overwrite": True},
        ctx,
    )
    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "aborted_version"
    assert "strict_base" in payload["conflict_reason"]


def test_move_file_records_write_scope_warning_for_out_of_scope_src() -> None:
    svc = _svc(
        move_result=_operation_result(
            success=True,
            paths=["/ws/other/a.py", "/ws/allowed/b.py"],
        )
    )
    ctx = _ctx(
        {
            "ci_service": svc,
            "agent_name": "developer",
            "repo_root": "/ws",
            "write_scope": ["allowed/"],
        }
    )
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/other/a.py", "dst_path": "/ws/allowed/b.py"},
        ctx,
    )
    assert result.is_error is False
    warnings = ctx.metadata.get("coordination_warnings") or []
    assert any(w.get("category") == "outside_write_scope" for w in warnings)


def test_move_file_in_scope_src_extends_write_scope_to_dst() -> None:
    svc = _svc(move_result=_operation_result(success=True, paths=["/ws/a.py", "/ws/b.py"]))
    original_scope = ["a.py"]
    ctx = _ctx(
        {
            "ci_service": svc,
            "agent_name": "developer",
            "repo_root": "/ws",
            "write_scope": original_scope,
        }
    )
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/a.py", "dst_path": "/ws/b.py"},
        ctx,
    )
    assert result.is_error is False
    warnings = ctx.metadata.get("coordination_warnings") or []
    assert not any(w.get("category") == "outside_write_scope" for w in warnings)
    assert "b.py" in (ctx.metadata.get("write_scope") or [])
    assert original_scope == ["a.py"]


def test_move_file_recursive_is_rejected_before_mutation() -> None:
    svc = _svc()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = _run(
        daytona_move_file,
        {"src_path": "/ws/pkg", "dst_path": "/ws/renamed", "recursive": True},
        ctx,
    )

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "failed"
    assert payload["conflict_reason"] == "recursive_unsupported"
    svc.move_file.assert_not_called()


def test_move_file_rejects_destination_inside_source() -> None:
    svc = _svc()
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = _run(
        daytona_move_file,
        {"src_path": "/ws/pkg", "dst_path": "/ws/pkg/nested"},
        ctx,
    )

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "failed"
    assert "inside source" in payload["message"]
    svc.move_file.assert_not_called()

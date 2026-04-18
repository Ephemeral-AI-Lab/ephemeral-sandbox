"""Tests for daytona_delete_file and daytona_move_file."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

from code_intelligence.types import EditResult, OperationResult
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.delete_move_tool import (
    daytona_delete_file,
    daytona_move_file,
)


def _ctx(metadata=None) -> ToolExecutionContext:
    metadata = dict(metadata or {})
    if "ci_service" in metadata and "daytona_sandbox" not in metadata:
        metadata["daytona_sandbox"] = SimpleNamespace()
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _run(tool, payload, ctx):
    return asyncio.run(tool.execute(tool.input_model(**payload), ctx))


def _ok_delete_svc():
    svc = MagicMock()
    svc.delete_file.return_value = OperationResult(
        success=True,
        status="committed",
        files=(
            EditResult(success=True, file_path="/ws/gone.py", message="Wrote file"),
        ),
    )
    return svc


def _ok_move_svc():
    svc = MagicMock()
    svc.move_file.return_value = OperationResult(
        success=True,
        status="committed",
        files=(
            EditResult(success=True, file_path="/ws/src.py", message="Wrote file"),
            EditResult(success=True, file_path="/ws/dst.py", message="Wrote file"),
        ),
    )
    return svc


# ---------------------------------------------------------------------------
# daytona_delete_file
# ---------------------------------------------------------------------------


def test_delete_file_success_routes_through_service() -> None:
    svc = _ok_delete_svc()
    ctx = _ctx({"ci_service": svc})

    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)

    payload = json.loads(result.output)
    assert result.is_error is False
    assert payload["status"] == "deleted"
    assert payload["paths"] == ["/ws/gone.py"]
    svc.delete_file.assert_called_once()
    call_kwargs = svc.delete_file.call_args
    assert call_kwargs.args[0] == "/ws/gone.py"


def test_delete_file_ci_required_when_service_missing() -> None:
    # No ci_service in context -> tool should surface ci_required error.
    ctx = _ctx({"daytona_sandbox": SimpleNamespace()})
    result = _run(daytona_delete_file, {"file_path": "/ws/gone.py"}, ctx)
    assert result.is_error is True
    assert "ci_required" in (result.metadata or {})


def test_delete_file_reports_not_found() -> None:
    svc = MagicMock()
    svc.delete_file.return_value = OperationResult(
        success=False,
        status="failed",
        files=(
            EditResult(
                success=False,
                file_path="/ws/missing.py",
                message="Path does not exist: /ws/missing.py",
            ),
        ),
        conflict_file=None,
        conflict_reason="not_found",
    )
    ctx = _ctx({"ci_service": svc})
    result = _run(daytona_delete_file, {"file_path": "/ws/missing.py"}, ctx)
    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "not_found"
    assert payload["conflict_reason"] == "not_found"


def test_delete_file_propagates_aborted_version() -> None:
    svc = MagicMock()
    svc.delete_file.return_value = OperationResult(
        success=False,
        status="aborted_version",
        files=(
            EditResult(
                success=False,
                file_path="/ws/drift.py",
                message="file content changed before delete",
                conflict=True,
                conflict_reason="aborted_version",
            ),
        ),
        conflict_file="/ws/drift.py",
        conflict_reason="file content changed before delete",
    )
    ctx = _ctx({"ci_service": svc})
    result = _run(daytona_delete_file, {"file_path": "/ws/drift.py"}, ctx)
    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "aborted_version"


def test_delete_file_records_write_scope_warning() -> None:
    """Out-of-scope delete still proceeds but records a coordination warning."""
    svc = _ok_delete_svc()
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
    svc.delete_file.assert_called_once()
    warnings = ctx.metadata.get("coordination_warnings") or []
    assert any(w.get("category") == "outside_write_scope" for w in warnings)


# ---------------------------------------------------------------------------
# daytona_move_file
# ---------------------------------------------------------------------------


def test_move_file_success_routes_through_service() -> None:
    svc = _ok_move_svc()
    ctx = _ctx({"ci_service": svc})
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py"},
        ctx,
    )
    payload = json.loads(result.output)
    assert result.is_error is False
    assert payload["status"] == "moved"
    assert payload["paths"] == ["/ws/src.py", "/ws/dst.py"]
    svc.move_file.assert_called_once()
    args = svc.move_file.call_args
    assert args.args[0] == "/ws/src.py"
    assert args.args[1] == "/ws/dst.py"
    assert args.kwargs["overwrite"] is False


def test_move_file_passes_overwrite_flag() -> None:
    svc = _ok_move_svc()
    ctx = _ctx({"ci_service": svc})
    _run(
        daytona_move_file,
        {"src_path": "/ws/a", "dst_path": "/ws/b", "overwrite": True},
        ctx,
    )
    assert svc.move_file.call_args.kwargs["overwrite"] is True


def test_move_file_dst_exists_without_overwrite() -> None:
    svc = MagicMock()
    svc.move_file.return_value = OperationResult(
        success=False,
        status="failed",
        files=(
            EditResult(
                success=False,
                file_path="/ws/dst.py",
                message="Destination exists: /ws/dst.py (pass overwrite=True to replace)",
            ),
        ),
        conflict_file="/ws/dst.py",
        conflict_reason="dst_exists",
    )
    ctx = _ctx({"ci_service": svc})
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py"},
        ctx,
    )
    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "dst_exists"


def test_move_file_aborted_version_surfaces_through_tool() -> None:
    svc = MagicMock()
    svc.move_file.return_value = OperationResult(
        success=False,
        status="aborted_version",
        files=(
            EditResult(
                success=False,
                file_path="/ws/dst.py",
                message="file content changed since base was captured",
                conflict=True,
                conflict_reason="aborted_version",
            ),
        ),
        conflict_file="/ws/dst.py",
        conflict_reason="file content changed since base was captured",
    )
    ctx = _ctx({"ci_service": svc})
    result = _run(
        daytona_move_file,
        {"src_path": "/ws/src.py", "dst_path": "/ws/dst.py", "overwrite": True},
        ctx,
    )
    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "aborted_version"


def test_move_file_records_write_scope_warning_for_out_of_scope_src() -> None:
    svc = _ok_move_svc()
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
    svc.move_file.assert_called_once()
    warnings = ctx.metadata.get("coordination_warnings") or []
    assert any(w.get("category") == "outside_write_scope" for w in warnings)

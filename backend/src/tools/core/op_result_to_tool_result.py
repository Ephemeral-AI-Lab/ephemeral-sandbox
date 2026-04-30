"""Shared :class:`OperationResult` → :class:`ToolResult` translation.

The Daytona mutation tools (edit, write, rename, delete, move) each map
the coordinator's ``OperationResult`` into the tool-side JSON payload
callers expect. They share exactly the same shape rules — status label,
conflict flags, message suffixes, files list — so the mapping lives in
one place instead of getting re-implemented per tool.
"""

from __future__ import annotations

import json
from typing import Any

from sandbox.code_intelligence.core.types import OperationResult
from tools.core.base import ToolResult

__all__ = ["operation_result_to_tool_result"]


def operation_result_to_tool_result(
    result: OperationResult,
    *,
    tool_name: str,
    success_status: str,
    primary_paths: list[str],
    warnings: list[str] | None = None,
    success_extra: dict[str, Any] | None = None,
    metadata_extra: dict[str, Any] | None = None,
) -> ToolResult:
    """Map an :class:`OperationResult` into a tool-facing :class:`ToolResult`.

    On success, emit a payload whose ``status`` is *success_status* (``"edited"``,
    ``"written"``, ``"renamed"``, ...) and ``paths`` is the union of the
    coordinator-reported file paths and *primary_paths* as a fallback.

    On failure, the payload carries ``status`` equal to the coordinator's
    abort class (``"aborted_version"``, ``"aborted_overlap"``, ...) or
    ``"failed"``; ``message`` pulls from ``conflict_reason`` when present.

    ``metadata_extra`` is merged into ``ToolResult.metadata`` on both branches
    so callers that funnel through :mod:`sandbox.commit` can
    inject uniform ``changed_paths`` / ``ambient_changed_paths`` /
    ``conflict_reason`` keys for downstream consumers.
    """
    paths = _paths_from_result(result, primary_paths)
    warnings_list = list(warnings or [])
    extra = dict(metadata_extra or {})
    if result.success:
        payload: dict[str, Any] = {
            "tool": tool_name,
            "status": success_status,
            "paths": paths,
            "warnings": warnings_list,
        }
        if success_extra:
            payload.update(success_extra)
        metadata: dict[str, Any] = {
            "tool": tool_name,
            "file_count": len(paths),
            "success_count": len(paths),
            "status": success_status,
        }
        metadata.update(extra)
        return ToolResult(output=json.dumps(payload), metadata=metadata)

    payload = {
        "tool": tool_name,
        "status": result.status or "failed",
        "paths": paths,
        "warnings": warnings_list,
        "conflict_file": result.conflict_file or "",
        "conflict_reason": result.conflict_reason or "",
        "message": _failure_message(result),
    }
    metadata = {
        "tool": tool_name,
        "file_count": len(paths),
        "success_count": 0,
        "status": result.status or "failed",
    }
    metadata.update(extra)
    return ToolResult(output=json.dumps(payload), is_error=True, metadata=metadata)


def _paths_from_result(
    result: OperationResult,
    fallback: list[str],
) -> list[str]:
    observed = [
        str(file_result.file_path)
        for file_result in result.files
        if str(file_result.file_path or "").strip()
    ]
    return observed or list(fallback)


def _failure_message(result: OperationResult) -> str:
    if result.files:
        first = result.files[0]
        if first.message:
            return str(first.message)
    if result.conflict_reason:
        return str(result.conflict_reason)
    return str(result.status or "operation failed")

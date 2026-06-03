"""Shared formatting for sandbox mutation tool results."""

from __future__ import annotations

import json
from typing import Any

from sandbox._shared.clock import normalize_timing_map
from sandbox._shared.models import GuardedResultBase
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult
from tools.sandbox._lib.tool_context import (
    sandbox_audit_metadata_from_tool_context,
    sandbox_repo_root_from_tool_context,
)


def mutation_tool_result(
    *,
    success: bool,
    success_status: str,
    paths: list[str],
    failure_status: str | None = None,
    conflict_reason: str | None = None,
    error: dict[str, object] | None = None,
    mutation_source: str = "",
    changed_path_kinds: dict[str, str] | None = None,
    success_extra: dict[str, Any] | None = None,
    timings: dict[str, float] | None = None,
    metadata_extra: dict[str, Any] | None = None,
) -> ToolResult:
    """Return the common JSON shape for file mutation tools."""
    status = (
        success_status
        if success
        else failure_status or _failure_status(conflict_reason)
    )
    metadata: dict[str, Any] = {
        "status": status,
        "changed_paths": paths,
        "conflict_reason": conflict_reason,
    }
    if error:
        metadata["error_kind"] = str(error.get("kind") or "")
        metadata["error"] = dict(error)
    if mutation_source:
        metadata["mutation_source"] = mutation_source
    if changed_path_kinds:
        metadata["changed_path_kinds"] = dict(changed_path_kinds)
    if timings:
        metadata["timings"] = normalize_timing_map(timings)
    metadata.update(metadata_extra or {})

    if success:
        payload: dict[str, Any] = {
            "status": status,
            "changed_paths": paths,
            "conflict_reason": None,
        }
        if mutation_source:
            payload["mutation_source"] = mutation_source
        if changed_path_kinds:
            payload["changed_path_kinds"] = dict(changed_path_kinds)
        payload.update(success_extra or {})
        return ToolResult(output=json.dumps(payload), metadata=metadata)

    payload = {
        "status": status,
        "changed_paths": paths,
        "conflict_reason": conflict_reason or "",
    }
    if error:
        payload["error"] = dict(error)
    if mutation_source:
        payload["mutation_source"] = mutation_source
    if changed_path_kinds:
        payload["changed_path_kinds"] = dict(changed_path_kinds)
    return ToolResult(
        output=json.dumps(payload),
        is_error=True,
        metadata=metadata,
    )


def project_file_mutation(
    result: GuardedResultBase,
    *,
    success_status: str,
    file_path: str,
    success_extra: dict[str, Any],
    context: ToolExecutionContextService,
) -> ToolResult:
    """Project a guarded file-mutation result into the standard ToolResult.

    Shared by ``write_file``/``edit_file``/``multi_edit``: the success/failure
    projection is identical except for ``success_status`` and the
    tool-specific ``success_extra`` (e.g. ``bytes_written`` vs ``applied_edits``).
    ``cwd`` and ``file_path`` are always added to the success payload.
    """
    paths = list(result.changed_paths)
    if result.success:
        return mutation_tool_result(
            success=True,
            success_status=success_status,
            paths=paths,
            success_extra={
                "cwd": sandbox_repo_root_from_tool_context(context),
                "file_path": file_path,
                **success_extra,
            },
            timings=result.timings,
            mutation_source=result.mutation_source,
            changed_path_kinds=dict(result.changed_path_kinds),
            metadata_extra=sandbox_audit_metadata_from_tool_context(context),
        )

    return mutation_tool_result(
        success=False,
        success_status=success_status,
        paths=paths,
        failure_status=result.status or None,
        conflict_reason=result.conflict_reason,
        error=result.error,
        mutation_source=result.mutation_source,
        changed_path_kinds=dict(result.changed_path_kinds),
        timings=result.timings,
        metadata_extra=sandbox_audit_metadata_from_tool_context(context),
    )


def _failure_status(conflict_reason: str | None) -> str:
    if conflict_reason in {"base_mismatch", "version_conflict", "drift"}:
        return "aborted_version"
    if conflict_reason in {"lock_conflict", "locked"}:
        return "aborted_lock"
    if conflict_reason in {"not_found", "missing"}:
        return "not_found"
    return "failed"


__all__ = ["mutation_tool_result", "project_file_mutation"]

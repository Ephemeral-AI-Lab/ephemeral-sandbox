"""Shared helpers for Daytona tools that mutate files."""

from __future__ import annotations

from typing import Any

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.ci_runtime import ci_write_required_result, get_ci_service


def ci_write_guard(
    context: ToolExecutionContextService,
    *,
    tool_name: str,
    path: str,
) -> ToolResult | None:
    """Return the standard CI-required error when writes are unavailable."""
    if get_ci_service(context) is None:
        return ci_write_required_result(tool_name, path)
    return None


def commit_metadata(change: Any, paths: list[str] | None = None) -> dict[str, Any]:
    """Return common metadata for file commit results."""
    changed_paths = list(change.changed_paths if paths is None else paths)
    return {
        "changed_paths": changed_paths,
        "ambient_changed_paths": list(change.ambient_changed_paths),
        "conflict_reason": change.conflict_reason,
    }


__all__ = ["ci_write_guard", "commit_metadata"]

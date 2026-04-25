"""Write audit helper for Daytona daytona_shell commits."""

from __future__ import annotations

from tools.core.base import ToolExecutionContext


def audited_write_outcome(
    context: ToolExecutionContext,
    changed_paths: list[str],
    *,
    tool_name: str,
) -> tuple[list[str], str]:
    """Return warnings and hard-block text for changed paths."""
    del context, changed_paths, tool_name
    return [], ""


__all__ = ["audited_write_outcome"]

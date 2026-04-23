"""Write-scope audit helper for Daytona daytona_shell commits."""

from __future__ import annotations

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import (
    _team_repo_write_error,
    _team_repo_write_warning,
)


def audited_write_outcome(
    context: ToolExecutionContext,
    changed_paths: list[str],
    *,
    tool_name: str,
) -> tuple[list[str], str]:
    """Return warnings and hard-block text for changed paths."""
    warnings: list[str] = []
    errors: list[str] = []
    for path in changed_paths:
        error = _team_repo_write_error(context, path, tool_name=tool_name)
        if error is not None:
            errors.append(error)
            continue
        warning = _team_repo_write_warning(context, path, tool_name=tool_name)
        if warning is not None:
            warnings.append(warning)
    return warnings, "\n".join(errors)


__all__ = ["audited_write_outcome"]

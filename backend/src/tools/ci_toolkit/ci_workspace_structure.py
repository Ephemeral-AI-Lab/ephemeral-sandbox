"""Code-intelligence workspace structure tool."""

from __future__ import annotations

from tools.ci_toolkit._query_runtime import (
    CiWorkspaceStructureInput,
    CiWorkspaceStructureOutput,
    run_ci_workspace_structure,
)
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool


@tool(
    name="ci_workspace_structure",
    description="List files and directories in the workspace, sorted by path.",
    short_description="List workspace files and directories.",
    input_model=CiWorkspaceStructureInput,
    output_model=CiWorkspaceStructureOutput,
)
async def ci_workspace_structure(
    path: str = "",
    max_depth: int = 3,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """List workspace file structure."""
    return await run_ci_workspace_structure(
        path=path,
        max_depth=max_depth,
        context=context,
    )


__all__ = ["ci_workspace_structure"]

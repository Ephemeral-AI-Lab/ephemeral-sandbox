"""Hover and diagnostics tools owned by the code intelligence toolkit."""

from __future__ import annotations

import json

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import get_ci_service
from tools.core.decorator import tool


def _ci_cwd(context: ToolExecutionContext) -> str | None:
    """Return the effective workspace root exposed to CI-backed tools."""
    return str(
        context.metadata.get("daytona_cwd")
        or context.metadata.get("ci_workspace_root")
        or context.cwd
        or ""
    ).strip() or None


@tool(
    name="ci_hover",
    description="Get type signature, return type, and docstring for a symbol at a file:line position — without reading the whole file. Use to check API contracts and parameter types before diving into implementation.",
    read_only=True,
)
async def ci_hover(
    file_path: str,
    line: int,
    character: int = 0,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Get type, signature, and docstring for a symbol."""
    svc = get_ci_service(context)
    if svc is None:
        return ToolResult(output="LSP not available", is_error=True)

    result = svc.hover(file_path, line, character)
    if result is None:
        return ToolResult(output=f"No hover information at {file_path}:{line}")

    return ToolResult(
        output=json.dumps(
            {
                "cwd": _ci_cwd(context) or "",
                "content": result.content,
                "language": result.language,
            }
        )
    )


@tool(
    name="ci_diagnostics",
    description="Check a file for syntax errors, type errors, and warnings after edits. Use before running full test suites to catch obvious mistakes early.",
    read_only=True,
)
async def ci_diagnostics(
    file_path: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Get syntax and semantic diagnostics for a file."""
    svc = get_ci_service(context)
    if svc is None:
        return ToolResult(output="LSP not available", is_error=True)

    results = svc.diagnostics(file_path)
    if not results:
        return ToolResult(
            output=json.dumps(
                {
                    "cwd": _ci_cwd(context) or "",
                    "file_path": file_path,
                    "diagnostics": [],
                    "clean": True,
                }
            )
        )

    diags = []
    for diag in results:
        diags.append(
            {
                "line": diag.line,
                "character": diag.character,
                "severity": (
                    diag.severity.value
                    if hasattr(diag.severity, "value")
                    else str(diag.severity)
                ),
                "message": diag.message,
                "source": diag.source,
            }
        )

    return ToolResult(
        output=json.dumps(
            {
                "cwd": _ci_cwd(context) or "",
                "file_path": file_path,
                "diagnostics": diags,
                "clean": False,
            }
        )
    )

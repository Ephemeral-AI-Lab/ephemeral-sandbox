"""Diagnostics tool owned by the code intelligence toolkit."""

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
    name="ci_diagnostics",
    description="Check a file for syntax errors, import errors, undefined names, and type warnings. Developers MUST call this on every edited file before signaling completion — a single unresolved NameError in a shared file cascades to every downstream test. Validators MUST call this on each scope_paths file before the full test suite.",
    short_description="Check a file for diagnostics.",
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

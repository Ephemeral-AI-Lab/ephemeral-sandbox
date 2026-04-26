"""Diagnostics tool owned by code intelligence tools."""

from __future__ import annotations

import json

from code_intelligence._async_bridge import run_sync_in_executor, use_sandbox_io_loop
from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.ci_runtime import get_ci_service
from tools.core.decorator import tool


def _ci_cwd(context: ToolExecutionContextService) -> str | None:
    """Return the effective workspace root exposed to CI-backed tools."""
    return str(
        context.get("repo_root")
        or context.get("ci_workspace_root")
        or context.cwd
        or ""
    ).strip() or None


class CiDiagnosticsInput(BaseModel):
    file_path: str = Field(
        ...,
        description="Path to the file to diagnose.",
    )


class DiagnosticOutput(BaseModel):
    line: int = Field(..., description="One-based diagnostic line number.")
    character: int = Field(..., description="Zero-based diagnostic character offset.")
    severity: str | int = Field(..., description="Diagnostic severity.")
    message: str = Field(..., description="Diagnostic message.")
    source: str | None = Field(default=None, description="Diagnostic source.")


class CiDiagnosticsOutput(BaseModel):
    cwd: str = Field(..., description="Effective workspace root.")
    file_path: str = Field(..., description="Diagnosed file path.")
    diagnostics: list[DiagnosticOutput] = Field(
        default_factory=list,
        description="Diagnostics returned for the file.",
    )
    clean: bool = Field(..., description="True when no diagnostics were found.")


@tool(
    name="ci_diagnostics",
    description="Returns syntax, import, name, and type diagnostics for one file.",
    short_description="Check a file for diagnostics.",
    input_model=CiDiagnosticsInput,
    output_model=CiDiagnosticsOutput,
)
async def ci_diagnostics(
    file_path: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Get syntax and semantic diagnostics for a file."""
    svc = get_ci_service(context)
    if svc is None:
        return ToolResult(output="LSP not available", is_error=True)

    try:
        with use_sandbox_io_loop():
            results = await run_sync_in_executor(svc.diagnostics, file_path)
    except Exception as exc:
        return ToolResult(
            output=f"LSP diagnostics unavailable: {exc}",
            is_error=True,
        )
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

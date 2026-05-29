"""Shell tool."""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import cast

from pydantic import BaseModel, Field

import sandbox.api as sandbox_api
from sandbox.shared.models import Intent
from sandbox.api import ShellRequest
from sandbox.shared.clock import normalize_timing_map
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.tool_context import (
    sandbox_audit_kwargs_from_tool_context,
    sandbox_caller_from_tool_context,
    sandbox_repo_root_from_tool_context,
    sandbox_audit_metadata_from_tool_context,
    sandbox_id_or_missing_error_result,
)
from tools._hooks.destructive_shell import (
    DestructiveGitShellPreHook,
    DestructiveShellPreHook,
)
from .prompt import get_shell_description

_SHELL_DEFAULT_TIMEOUT = 900


class ShellInput(BaseModel):
    """Input for shell."""

    command: str = Field(
        ...,
        min_length=1,
        description="Shell command to run for tests, builds, or verification.",
    )
    timeout: int = Field(
        default=_SHELL_DEFAULT_TIMEOUT,
        description="Shell command timeout in seconds.",
    )


class ShellOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    status: str = Field(..., description="Execution status: ok or error.")
    changed_paths: list[str] = Field(default_factory=list, description="Files changed by the command.")
    changed_path_kinds: dict[str, str] = Field(
        default_factory=dict,
        description="Captured changed paths keyed to write/delete/symlink/opaque_dir.",
    )
    mutation_source: str = Field(default="", description="Mutation source tag.")
    conflict_reason: str | None = Field(default=None, description="Conflict reason when auditing failed.")
    command: str = Field(..., description="Shell command that was run.")
    exit_code: int | str = Field(..., description="Command exit code.")
    stdout: str = Field(..., description="Captured stdout.")
    stderr: str = Field(..., description="Captured stderr.")
    error: str = Field(default="", description="Error detail when status is error.")


def _format_transport_exception(exc: Exception) -> str:
    detail = str(exc).strip() or repr(exc)
    if detail.rstrip().endswith(":"):
        detail = f"{detail} (no additional detail from sandbox SDK)"
    return f"{detail} [exception_type={type(exc).__name__}]"


def _format_execution_failure(
    exc: Exception,
    *,
    operation: str,
    command: str | None = None,
    timeout: int | None = None,
) -> str:
    parts = [
        "Execution failed:",
        _format_transport_exception(exc),
        f"operation={operation}",
    ]
    if timeout is not None:
        parts.append(f"timeout={timeout}s")
    if command:
        preview = " ".join(command.split())
        if len(preview) > 240:
            preview = f"{preview[:237]}..."
        parts.append(f"command={preview!r}")
    return " ".join(parts)


def _build_shell_tool_result(
    *,
    context: ToolExecutionContextService,
    status: str,
    command: str,
    exit_code: int | str,
    stdout: str,
    stderr: str,
    changed_paths: list[str],
    changed_path_kinds: dict[str, str] | None,
    mutation_source: str,
    conflict_reason: str | None,
    error: str = "",
    error_kind: str = "",
    timings: dict[str, float] | None = None,
) -> ToolResult:
    metadata: dict[str, object] = {
        "status": status,
        "changed_paths": changed_paths,
        "changed_path_kinds": dict(changed_path_kinds or {}),
        "mutation_source": mutation_source,
        "conflict_reason": conflict_reason,
    }
    if error_kind:
        metadata["error_kind"] = error_kind
    if timings:
        metadata["timings"] = normalize_timing_map(
            cast(Mapping[object, object], timings)
        )
    metadata.update(sandbox_audit_metadata_from_tool_context(context))
    return ToolResult(
        output=json.dumps(
            {
                "cwd": sandbox_repo_root_from_tool_context(context),
                "status": status,
                "changed_paths": changed_paths,
                "changed_path_kinds": dict(changed_path_kinds or {}),
                "mutation_source": mutation_source,
                "conflict_reason": conflict_reason,
                "command": command,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "error": error if error else "",
            }
        ),
        is_error=status == "error",
        metadata=metadata,
    )


@tool(
    name="shell",
    description=get_shell_description(),
    short_description="Run a shell command from the repo root.",
    input_model=ShellInput,
    output_model=ShellOutput,
    intent=Intent.WRITE_ALLOWED,
    pre_hooks=(DestructiveGitShellPreHook(), DestructiveShellPreHook()),
    background="optional",
)
async def shell(
    command: str,
    timeout: int = _SHELL_DEFAULT_TIMEOUT,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Run a shell command."""
    if not command or not command.strip():
        return ToolResult(output="`command` must be a non-empty string.", is_error=True)

    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error

    try:
        result = await sandbox_api.shell(
            sandbox_id,
            ShellRequest(
                invocation_id=str(context.get("sandbox_invocation_id") or ""),
                command=command,
                cwd=sandbox_repo_root_from_tool_context(context) or None,
                timeout=timeout,
                caller=sandbox_caller_from_tool_context(context),
                description="shell",
                background=bool(context.get("background_task_id")),
            ),
            **sandbox_audit_kwargs_from_tool_context(context),
        )
    except Exception as exc:
        return ToolResult(
            output=_format_execution_failure(
                exc,
                operation="shell",
                command=command,
                timeout=timeout,
            ),
            is_error=True,
        )

    changed_paths = sorted(
        {str(path) for path in result.changed_paths if str(path or "").strip()}
    )
    changed_path_kinds = dict(result.changed_path_kinds)
    error_payload = dict(result.error or {})
    is_error = result.exit_code != 0 or not result.success
    if not result.success and result.exit_code == 0:
        error_detail = (
            f"sandbox commit aborted: {result.conflict_reason or 'unknown reason'}"
        )
    elif error_payload:
        error_detail = str(error_payload.get("message") or error_payload.get("kind") or "")
    elif result.exit_code != 0:
        error_detail = result.stderr or result.stdout or ""
    else:
        error_detail = ""
    return _build_shell_tool_result(
        context=context,
        status="error" if is_error else "ok",
        command=command,
        exit_code=result.exit_code,
        stdout=result.stdout,
        stderr=result.stderr,
        changed_paths=changed_paths,
        changed_path_kinds=changed_path_kinds,
        mutation_source=result.mutation_source,
        conflict_reason=result.conflict_reason,
        error=error_detail,
        error_kind=str(error_payload.get("kind") or ""),
        timings=result.timings,
    )


__all__ = ["shell", "_build_shell_tool_result"]

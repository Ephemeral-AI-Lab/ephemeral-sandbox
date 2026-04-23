"""Run shell commands in the Daytona repo root.

Coordinated team lanes must not use daytona_shell to mutate package or
environment state; missing dependencies are workflow evidence for replanning.
"""

from __future__ import annotations

import json
import shlex
from typing import Callable

from pydantic import BaseModel, Field

from code_intelligence.tuning import CODE_INTELLIGENCE_TUNING
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import ci_required_result, get_ci_service
from tools.core.decorator import tool
from tools.daytona_toolkit._commit import submit_shell_cmd
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _format_shell_stdout,
    _get_cwd,
    _recover_sandbox,
    _require_sandbox,
    _wrap_bash_command,
)

_SHELL_DEFAULT_TIMEOUT = CODE_INTELLIGENCE_TUNING.shell_default_timeout


class DaytonaShellInput(BaseModel):
    """Input schema for the daytona_shell tool."""

    command: str = Field(
        ...,
        min_length=1,
        description=(
            "Shell command to run from the repo root. Use for tests, builds, "
            "and verification. Do not prefix with host paths like /Users/...; "
            "the sandbox repo root is usually /testbed. Output is captured "
            "automatically. In coordinated team lanes, do not run package or "
            "environment mutation commands such as pip install, uv sync, "
            "npm install, or equivalent install/add/sync/update operations."
        ),
    )
    timeout: int = Field(
        default=_SHELL_DEFAULT_TIMEOUT,
        description="Shell command timeout in seconds.",
    )


class DaytonaShellCommandOutput(BaseModel):
    command: str = Field(..., description="Shell command that was run.")
    exit_code: int | str = Field(..., description="Command exit code.")
    stdout: str = Field(..., description="Captured stdout.")
    stderr: str = Field(..., description="Captured stderr.")


class DaytonaShellOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    status: str = Field(..., description="Execution status: ok or error.")
    files_written: int = Field(
        ...,
        description="Number of audited process file writes observed.",
    )
    shells_run: int = Field(..., description="Number of shell commands executed.")
    shell_summaries: list[str] = Field(
        default_factory=list,
        description="Compact summaries of the shell command.",
    )
    shell_outputs: list[DaytonaShellCommandOutput] = Field(
        default_factory=list,
        description="Captured output for the shell command.",
    )
    warnings: list[str] = Field(default_factory=list, description="Non-fatal warnings.")
    error: str = Field(default="", description="Error detail when status is error.")


def _format_transport_exception(exc: Exception) -> str:
    detail = str(exc).strip()
    if not detail:
        detail = repr(exc)
    if detail.rstrip().endswith(":"):
        detail = f"{detail} (no additional detail from Daytona SDK)"
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


def _progress_callback(context: ToolExecutionContext) -> Callable[[str], None] | None:
    callback = context.metadata.get("on_progress_line")
    return callback if callable(callback) else None


async def _exec_shell_command(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    command: str,
    cwd: str | None,
    timeout: int,
    attribute_changes: bool,
) -> dict[str, object]:
    if get_ci_service(context) is None:
        raise RuntimeError("Code intelligence service is unavailable")

    wrapped_command = command if not cwd else f"cd {shlex.quote(cwd)} && {command}"
    change = await submit_shell_cmd(
        context,
        command=_wrap_bash_command(wrapped_command),
        description="daytona_shell",
        timeout=timeout,
        sandbox=sandbox,
        attribute_changes=attribute_changes,
        on_progress_line=_progress_callback(context),
    )
    response = change.raw
    stdout = getattr(response, "result", "") or ""
    fallback_exit_code = getattr(response, "exit_code", None)
    cleaned_stdout, exit_code = _extract_exit_code(
        stdout,
        fallback_exit_code=fallback_exit_code,
    )
    formatted_stdout = _format_shell_stdout(cleaned_stdout, exit_code=exit_code)
    return {
        "command": command,
        "stdout": formatted_stdout,
        "stderr": formatted_stdout if exit_code != 0 else "",
        "exit_code": exit_code,
        "changed_paths": list(change.changed_paths),
        "ambient_changed_paths": list(change.ambient_changed_paths),
        "audit_success": bool(change.success),
        "audit_conflict_reason": change.conflict_reason,
    }


async def _run_shell_with_recovery(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    command: str,
    cwd: str | None,
    timeout: int,
    attribute_changes: bool,
) -> tuple[dict[str, object] | None, object, ToolResult | None]:
    try:
        return (
            await _exec_shell_command(
                context,
                sandbox,
                command=command,
                cwd=cwd,
                timeout=timeout,
                attribute_changes=attribute_changes,
            ),
            sandbox,
            None,
        )
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            return (
                await _exec_shell_command(
                    context,
                    sandbox,
                    command=command,
                    cwd=cwd,
                    timeout=timeout,
                    attribute_changes=attribute_changes,
                ),
                sandbox,
                None,
            )
        except Exception as recovery_exc:
            return (
                None,
                sandbox,
                ToolResult(
                    output=_format_execution_failure(
                        recovery_exc,
                        operation="daytona_shell",
                        command=command,
                        timeout=timeout,
                    ),
                    is_error=True,
                ),
            )


def _build_tool_output(
    *,
    context: ToolExecutionContext,
    status: str,
    files_written: int,
    shells: list[dict[str, object]],
    warnings: list[str],
    error: str = "",
    changed_paths: list[str] | None = None,
    ambient_changed_paths: list[str] | None = None,
) -> ToolResult:
    shell_summaries: list[str] = []
    shell_outputs: list[dict[str, object]] = []
    for shell_result in shells[:3]:
        command = str(shell_result.get("command", "") or "")
        exit_code = shell_result.get("exit_code", "?")
        try:
            exit_code_int = int(exit_code)
        except (TypeError, ValueError):
            exit_code_int = 1
        stdout = _format_shell_stdout(
            str(shell_result.get("stdout", "") or ""),
            exit_code=exit_code_int,
        )
        stderr = _format_shell_stdout(
            str(shell_result.get("stderr", "") or ""),
            exit_code=exit_code_int,
        )
        shell_summaries.append(f"$ {command[:80]} -> exit {exit_code}")
        shell_outputs.append(
            {
                "command": command,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
            }
        )

    is_error = status == "error"

    return ToolResult(
        output=json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "status": status,
                "files_written": files_written,
                "shells_run": len(shells),
                "shell_summaries": shell_summaries,
                "shell_outputs": shell_outputs,
                "warnings": warnings,
                "error": error[:500] if error else "",
            }
        ),
        is_error=is_error,
        metadata={
            "status": status,
            "files_written": files_written,
            "shells_run": len(shells),
            "changed_paths": list(changed_paths or []),
            "ambient_changed_paths": list(ambient_changed_paths or []),
        },
    )


def _ci_required_result() -> ToolResult:
    return ci_required_result(
        "daytona_shell",
        "Shell command execution is disabled without CI service.",
    )


def _shell_result_error_detail(shell_result: dict[str, object]) -> str:
    return str(shell_result.get("stderr", "") or shell_result.get("stdout", "") or "")


def _changed_paths_from_shell(shell_result: dict[str, object]) -> list[str]:
    raw = shell_result.get("changed_paths")
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


def _ambient_changed_paths_from_shell(shell_result: dict[str, object]) -> list[str]:
    raw = shell_result.get("ambient_changed_paths")
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


@tool(
    name="daytona_shell",
    description=(
        "Run a shell command in the Daytona sandbox. Use for tests, builds, "
        "and verification. Commands start at the sandbox repo root, usually "
        "`/testbed`; never prefix with host paths like `/Users/...`. Output "
        "is captured automatically. In coordinated team lanes, do not use "
        "this for package or environment mutation commands such as "
        "`pip install`, `uv sync`, `npm install`, or equivalent "
        "install/add/sync/update operations. Do not use this for file writes, "
        "moves, deletes, or file-content reads; use the file, search, rename, "
        "delete, or move tools instead."
    ),
    short_description="Run a shell command from the repo root.",
    input_model=DaytonaShellInput,
    output_model=DaytonaShellOutput,
    background="optional",
)
async def daytona_shell(
    command: str,
    timeout: int = _SHELL_DEFAULT_TIMEOUT,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Run a shell command in the Daytona sandbox."""
    if not command or not command.strip():
        return ToolResult(output="`command` must be a non-empty string.", is_error=True)

    repo_cwd = _get_cwd(context)

    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)

    if get_ci_service(context) is None:
        return _ci_required_result()

    shell_result, sandbox, tool_error = await _run_shell_with_recovery(
        context,
        sandbox,
        command=command,
        cwd=repo_cwd,
        timeout=timeout,
        attribute_changes=True,
    )
    if tool_error is not None:
        return tool_error
    assert shell_result is not None
    exit_code = int(shell_result.get("exit_code", 1))
    audit_success = bool(shell_result.get("audit_success", True))
    audit_conflict = shell_result.get("audit_conflict_reason") or ""
    changed_paths = _changed_paths_from_shell(shell_result)
    ambient_changed_paths = _ambient_changed_paths_from_shell(shell_result)
    is_error = exit_code != 0 or not audit_success
    if not audit_success and exit_code == 0:
        error_detail = (
            f"sandbox commit aborted: {audit_conflict or 'unknown reason'}"
        )
    elif exit_code != 0:
        error_detail = _shell_result_error_detail(shell_result)
    else:
        error_detail = ""
    return _build_tool_output(
        context=context,
        status="ok" if not is_error else "error",
        files_written=len(changed_paths),
        shells=[shell_result],
        warnings=[],
        error=error_detail,
        changed_paths=changed_paths,
        ambient_changed_paths=ambient_changed_paths,
    )

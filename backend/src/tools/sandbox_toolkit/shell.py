"""Shell tool."""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from sandbox.api.models import ShellRequest
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.core.sandbox_session import (
    actor_from_context,
    get_repo_root,
    sandbox_api_or_error,
    sandbox_id_or_error,
)
from tools.sandbox_toolkit._shell_prehooks import (
    DestructiveGitShellPreHook,
    DestructiveShellPreHook,
)

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


class ShellCommandOutput(BaseModel):
    command: str = Field(..., description="Shell command that was run.")
    exit_code: int | str = Field(..., description="Command exit code.")
    stdout: str = Field(..., description="Captured stdout.")
    stderr: str = Field(..., description="Captured stderr.")


class ShellOutput(BaseModel):
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
    shell_outputs: list[ShellCommandOutput] = Field(
        default_factory=list,
        description="Captured output for the shell command.",
    )
    warnings: list[str] = Field(default_factory=list, description="Non-fatal warnings.")
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


def _build_tool_output(
    *,
    context: ToolExecutionContextService,
    status: str,
    files_written: int,
    shells: list[dict[str, object]],
    warnings: list[str],
    error: str = "",
    changed_paths: list[str] | None = None,
) -> ToolResult:
    shell_summaries: list[str] = []
    shell_outputs: list[dict[str, object]] = []
    for shell_result in shells:
        command = str(shell_result.get("command", "") or "")
        exit_code = shell_result.get("exit_code", "?")
        stdout = str(shell_result.get("stdout", "") or "")
        stderr = str(shell_result.get("stderr", "") or "")
        shell_summaries.append(f"$ {command} -> exit {exit_code}")
        shell_outputs.append(
            {
                "command": command,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
            }
        )

    return ToolResult(
        output=json.dumps(
            {
                "cwd": get_repo_root(context),
                "status": status,
                "files_written": files_written,
                "shells_run": len(shells),
                "shell_summaries": shell_summaries,
                "shell_outputs": shell_outputs,
                "warnings": warnings,
                "error": error if error else "",
            }
        ),
        is_error=status == "error",
        metadata={
            "status": status,
            "files_written": files_written,
            "shells_run": len(shells),
            "changed_paths": list(changed_paths or []),
        },
    )


def _shell_result_error_detail(shell_result: dict[str, object]) -> str:
    return str(shell_result.get("stderr", "") or shell_result.get("stdout", "") or "")


def _paths_from_shell(shell_result: dict[str, object], key: str) -> list[str]:
    raw = shell_result.get(key)
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


@tool(
    name="shell",
    description=(
        "Run a single bash command from the sandbox repo root. Captures stdout, stderr, and "
        "exit code, and audits any file writes the command performs.\n\n"
        "Use this when:\n"
        "- You need to run tests, builds, linters, type-checkers, or other tooling "
        "(`pytest`, `make build`, `npm test`, `ruff check`).\n"
        "- You need a capability not exposed as a dedicated tool (git operations, "
        "pip/uv/npm install, generating files via codegen).\n"
        "- You're verifying environment state (`which python`, `git status`, `ls -la`).\n\n"
        "Prefer dedicated tools when applicable:\n"
        "- File reads -> `read_file`, not `cat`.\n"
        "- File mutations -> `write_file`/`edit_file`. The dedicated tools produce cleaner "
        "audit trails and structured errors.\n"
        "- Use `shell` for tasks the dedicated tools genuinely cannot do (filename search "
        "via `find`/`ls`, content search via `grep`/`rg`, moves via `mv`, deletes via `rm`).\n\n"
        "Do NOT use for:\n"
        "- Long-running interactive processes (REPLs, watchers, dev servers). Each call is "
        "one-shot and bounded by `timeout`.\n"
        "- Background daemons. There is no persistent shell session between calls; cwd resets "
        "to the repo root each time.\n"
        "- Streaming progress to the user — only the final captured output is returned.\n\n"
        "Capabilities and constraints:\n"
        "- Runs as bash, with the repo root as cwd.\n"
        "- `timeout` (seconds) bounds the run; default is 900.\n"
        "- Writes performed by the command are tracked. A command that exits 0 but writes "
        'outside the audited boundary returns is_error=True with "sandbox commit aborted: ...".\n'
        "- No environment leakage between calls — set env vars inline (`FOO=bar cmd ...`).\n"
        "- No interactive input — use non-interactive flags (`--yes`, `--non-interactive`, "
        "`--no-input`).\n\n"
        "Output shape:\n"
        '- `status`: "ok" | "error".\n'
        "- `shell_outputs[0]`: the captured `command`, `exit_code`, `stdout`, `stderr`.\n"
        "- `files_written`: count of audited writes the command performed.\n"
        '- `error`: populated when status is "error" — combines exit-code failures and audit '
        "conflicts.\n\n"
        "Common pitfalls:\n"
        "- Quoting: prefer single quotes around regexes and arguments containing $.\n"
        "- Pipelines: pipe failures are masked unless you `set -o pipefail` inline.\n"
        "- Background `&`: don't — the audit will not see the result, and you have no way to "
        "wait."
    ),
    short_description="Run a shell command from the repo root.",
    input_model=ShellInput,
    output_model=ShellOutput,
    background="optional",
    pre_hooks=(DestructiveGitShellPreHook(), DestructiveShellPreHook()),
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

    sandbox_id, sandbox_id_error = sandbox_id_or_error(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    api, api_error = sandbox_api_or_error(context, tool_name="shell")
    if api_error is not None:
        return api_error

    try:
        result = await api.shell(
            sandbox_id,
            ShellRequest(
                command=command,
                cwd=get_repo_root(context) or None,
                timeout=timeout,
                actor=actor_from_context(context),
                description="shell",
                attribute_changes=True,
            ),
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

    shell_result: dict[str, object] = {
        "command": command,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "exit_code": result.exit_code,
        "changed_paths": list(result.changed_paths),
        "success": result.success,
        "conflict_reason": result.conflict_reason,
    }
    changed_paths = _paths_from_shell(shell_result, "changed_paths")
    is_error = result.exit_code != 0 or not result.success
    if not result.success and result.exit_code == 0:
        error_detail = (
            f"sandbox commit aborted: {result.conflict_reason or 'unknown reason'}"
        )
    elif result.exit_code != 0:
        error_detail = _shell_result_error_detail(shell_result)
    else:
        error_detail = ""
    return _build_tool_output(
        context=context,
        status="error" if is_error else "ok",
        files_written=len(changed_paths),
        shells=[shell_result],
        warnings=list(result.warnings),
        error=error_detail,
        changed_paths=changed_paths,
    )


__all__ = ["shell", "_build_tool_output"]

"""CodeAct tool - shell or Python execution in the Daytona sandbox."""

from __future__ import annotations

import base64
import json
import re
import shlex
import uuid
from collections import OrderedDict
from typing import Any, Literal

from pydantic import BaseModel, Field
from pydantic.json_schema import GenerateJsonSchema

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _get_cwd,
    _recover_sandbox,
    _require_sandbox,
    _upload_file_compat,
    _wrap_bash_command,
    is_coordinated_team_agent,
)
from tools.daytona_toolkit.ci_integration import destructive_shell_command_error
from tools.daytona_toolkit.codeact_transaction import (
    cleanup_codeact_transaction,
    collect_transaction_changes,
    commit_transaction_changes,
    create_codeact_transaction,
)

_DESTRUCTIVE_GIT_PATTERN = re.compile(
    r"git\s+(stash|reset\s+--hard|checkout\s+--\s|checkout\s+\.\s*$|clean\s+-[fd])",
    flags=re.IGNORECASE,
)


class DaytonaCodeActInput(BaseModel):
    """Custom CodeAct input schema.

    Keep runtime parsing permissive so existing callers still flow through
    ``_resolve_mode()``, but publish a stricter JSON schema to the model.
    Anthropic-compatible models will otherwise happily emit explicit JSON
    ``null`` for optional string params and spin on empty CodeAct calls.
    """

    mode: Literal["python", "shell"] | None = Field(
        default=None,
        description=(
            "Optional explicit mode. Omit unless you need to force shell or "
            "python execution."
        ),
    )
    code: str | None = Field(
        default=None,
        description=(
            "Python code to execute. Use for multi-step helper flows; do not "
            "set alongside `command`."
        ),
    )
    command: str | None = Field(
        default=None,
        description=(
            "Shell command to execute directly. Preferred for tests, builds, "
            "and verification; do not set alongside `code`."
        ),
    )
    timeout: int = Field(
        default=900,
        description="Timeout in seconds for shell mode execution.",
    )

    @classmethod
    def model_json_schema(
        cls,
        by_alias: bool = True,
        ref_template: str = "#/$defs/{model}",
        schema_generator: type[GenerateJsonSchema] = GenerateJsonSchema,
        mode: str = "validation",
    ) -> dict[str, Any]:
        schema = super().model_json_schema(
            by_alias=by_alias,
            ref_template=ref_template,
            schema_generator=schema_generator,
            mode=mode,
        )
        props = schema.get("properties", {})

        def _strip_null_variant(name: str, expected_type: str) -> None:
            prop = props.get(name)
            if not isinstance(prop, dict):
                return
            cleaned: dict[str, Any] | None = None
            for variant in prop.get("anyOf", []):
                if isinstance(variant, dict) and variant.get("type") == expected_type:
                    cleaned = dict(variant)
                    break
            if cleaned is None:
                return
            if "title" in prop:
                cleaned["title"] = prop["title"]
            if "description" in prop:
                cleaned["description"] = prop["description"]
            cleaned.pop("default", None)
            if expected_type == "string":
                cleaned["minLength"] = max(int(cleaned.get("minLength", 1) or 1), 1)
            props[name] = cleaned

        _strip_null_variant("mode", "string")
        _strip_null_variant("code", "string")
        _strip_null_variant("command", "string")

        schema["oneOf"] = [
            {"required": ["command"]},
            {"required": ["code"]},
        ]
        return schema


def _destructive_git_command_error(command: str) -> str | None:
    if _DESTRUCTIVE_GIT_PATTERN.search(command or ""):
        return (
            "BLOCKED: destructive git commands (stash, reset --hard, checkout --, clean) "
            "are forbidden. They destroy other agents' work and bypass OCC. "
            "Use targeted edit tools instead."
        )
    return None


def _format_codeact_error(
    *,
    stdout: str,
    manifest_error: str = "",
) -> str:
    detail = manifest_error.strip() or stdout[:4000]
    lines = ["CodeAct execution error:"]
    if detail:
        lines.append(detail)
    if "blocked in codeact" in detail or "subprocess" in detail or "os.system" in detail:
        lines.append(
            "Use `daytona_codeact(command=\"...\")` or `shell(\"...\")` inside Python mode; "
            "do not import `subprocess` or call `os.system()`."
        )
    return "\n".join(lines)


_WRAPPER_TEMPLATE = r'''
import base64, hashlib, importlib, json, os, re, subprocess, traceback

_RUN_ID = "{run_id}"
_MANIFEST = {{"reads": [], "writes": [], "shells": [], "status": "ok", "error": ""}}
_CODEACT_CWD = {codeact_cwd}
_ENFORCE_TEAM_SHELL_POLICY = {enforce_team_shell_policy}
_USER_LOCAL_BIN_EXPORT = 'export PATH="$HOME/.local/bin:$PATH"'
_PROJECT_VENV_BIN_EXPORT = 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi'
_PYTHON3_SHIM = 'if command -v python3 >/dev/null 2>&1; then python() {{ command python3 "$@"; }}; fi'
_BLOCKED_MODULES = frozenset({{"subprocess", "shutil"}})
_DESTRUCTIVE_GIT_PATTERN = re.compile(
    r"git\s+(stash|reset\s+--hard|checkout\s+--\s|checkout\s+\.\s*$|clean\s+-[fd])",
    flags=re.IGNORECASE,
)
_DESTRUCTIVE_SHELL_PATTERN = re.compile(
    r"(?:^|[;&|]\s*)(?:"
    r"rm\s+(?:-\S*[rR]\S*\s+|--recursive\s+)(?:/(?:testbed|workspace|home|opt|usr|var|etc|tmp)\b|/\s|/\.\.|\.\.)"
    r"|mv\s+/(?:testbed|workspace|home|opt|usr|var|etc)(?:/[^/\s]*)?(?:\s|$)"
    r"|chmod\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/"
    r"|chown\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/"
    r"|rm\s+-\S*[rR]\S*\s+\.\s*$"
    r"|mkfs\b|dd\s+.*of=/"
    r")",
    flags=re.IGNORECASE,
)

def _normalize_path(path):
    if os.path.isabs(path):
        return path
    return os.path.abspath(path)

def read(path):
    resolved = _normalize_path(path)
    with open(resolved, "r", encoding="utf-8") as f:
        content = f.read()
    h = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    _MANIFEST["reads"].append({{"path": resolved, "hash": h}})
    return content

def write(path, content):
    resolved = _normalize_path(path)
    parent = os.path.dirname(resolved)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(resolved, "w", encoding="utf-8") as f:
        f.write(content)
    _MANIFEST["writes"].append({{"path": resolved, "content": content}})
    return resolved

def _block_shell_command(command, message):
    _MANIFEST["shells"].append(
        {{
            "command": command,
            "stdout": "",
            "stderr": message,
            "exit_code": -1,
            "blocked": True,
        }}
    )
    raise RuntimeError(message)

def shell(command, timeout=900):
    if _ENFORCE_TEAM_SHELL_POLICY and "2>&1" in (command or ""):
        _block_shell_command(
            command,
            "CodeAct policy error: do not append `2>&1`; stdout/stderr are already captured.",
        )
    if _DESTRUCTIVE_GIT_PATTERN.search(command or ""):
        _block_shell_command(
            command,
            "BLOCKED: destructive git commands (stash, reset --hard, checkout --, clean) "
            "are forbidden. They destroy other agents' work and bypass OCC. "
            "Use targeted edit tools instead.",
        )
    if _DESTRUCTIVE_SHELL_PATTERN.search(command or ""):
        _block_shell_command(
            command,
            "BLOCKED: destructive shell command that targets workspace or system "
            "directories is forbidden. Use targeted file operations instead.",
        )
    try:
        wrapped = f"{{_USER_LOCAL_BIN_EXPORT}} && {{_PROJECT_VENV_BIN_EXPORT}} && {{_PYTHON3_SHIM}} && {{command}}"
        proc = subprocess.run(
            ["env", "-u", "LC_ALL", "bash", "-o", "pipefail", "-lc", wrapped],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=_CODEACT_CWD or None,
        )
        result = {{
            "command": command,
            "stdout": proc.stdout,
            "stderr": proc.stderr,
            "exit_code": proc.returncode,
        }}
    except subprocess.TimeoutExpired:
        result = {{
            "command": command,
            "stdout": "",
            "stderr": "timeout",
            "exit_code": -1,
        }}
    except Exception as exc:
        result = {{
            "command": command,
            "stdout": "",
            "stderr": str(exc),
            "exit_code": -1,
        }}
    _MANIFEST["shells"].append(result)
    return result

import builtins as _builtins_mod
_real_import = _builtins_mod.__import__

def _guarded_import(name, *args, **kwargs):
    top = name.split(".")[0]
    if top in _BLOCKED_MODULES:
        raise ImportError(
            f"import {{name!r}} is blocked in codeact. "
            "Use daytona_codeact shell mode for commands and read()/write() for file I/O."
        )
    return _real_import(name, *args, **kwargs)

_sandbox_builtins = dict(vars(_builtins_mod))
_sandbox_builtins["__import__"] = _guarded_import

_real_import_module = importlib.import_module

def _guarded_import_module(name, package=None):
    top = name.split(".")[0]
    if top in _BLOCKED_MODULES:
        raise ImportError(
            f"import {{name!r}} is blocked in codeact. "
            "Use daytona_codeact shell mode for commands and read()/write() for file I/O."
        )
    return _real_import_module(name, package)

importlib.import_module = _guarded_import_module

if _ENFORCE_TEAM_SHELL_POLICY:
    def _blocked_os_process(*args, **kwargs):
        raise RuntimeError(
            "CodeAct policy error: coordinated team lanes must use `daytona_codeact` shell mode "
            "or `shell(\"...\")` inside Python mode for repo commands. Replace `os.system()`/"
            "`os.popen()` wrappers."
        )

    os.system = _blocked_os_process
    os.popen = _blocked_os_process

try:
    _CODE = base64.b64decode("{code_b64}").decode("utf-8")
    exec(
        _CODE,
        {{"read": read, "write": write, "shell": shell, "__name__": "__codeact__", "__builtins__": _sandbox_builtins}},
    )
except Exception:
    _MANIFEST["status"] = "error"
    _MANIFEST["error"] = traceback.format_exc()[:2000]

with open("/tmp/codeact-{run_id}.json", "w", encoding="utf-8") as f:
    json.dump(_MANIFEST, f)

print(json.dumps({{"manifest": "/tmp/codeact-{run_id}.json", "status": _MANIFEST["status"]}}))
'''


def _build_wrapper(
    code: str,
    *,
    enforce_team_shell_policy: bool,
    run_id: str,
    cwd: str | None,
) -> str:
    code_b64 = base64.b64encode(code.encode("utf-8")).decode("ascii")
    return _WRAPPER_TEMPLATE.format(
        run_id=run_id,
        code_b64=code_b64,
        codeact_cwd=json.dumps(cwd) if cwd else "None",
        enforce_team_shell_policy="True" if enforce_team_shell_policy else "False",
    )


def _build_exec_command(script_path: str, *, cwd: str | None) -> str:
    command = f"python3 {script_path}"
    if cwd:
        command = f"cd {json.dumps(cwd)} && {command}"
    return _wrap_bash_command(command)


def _coalesce_manifest_writes(writes: list[dict[str, object]]) -> list[str]:
    final_writes: OrderedDict[str, None] = OrderedDict()
    for item in writes:
        path = str(item.get("path", "") or "")
        if not path:
            continue
        if path in final_writes:
            del final_writes[path]
        final_writes[path] = None
    return list(final_writes.keys())


def _resolve_mode(
    *,
    mode: Literal["python", "shell"] | None,
    code: str | None,
    command: str | None,
) -> tuple[Literal["python", "shell"] | None, str | None]:
    has_code = isinstance(code, str) and bool(code.strip())
    has_command = isinstance(command, str) and bool(command.strip())
    if mode == "python":
        if not has_code or has_command:
            return None, "`mode=\"python\"` requires `code` and forbids `command`."
        return "python", None
    if mode == "shell":
        if not has_command or has_code:
            return None, "`mode=\"shell\"` requires `command` and forbids `code`."
        return "shell", None
    if has_code and has_command:
        return None, "Provide either `code` or `command`, not both."
    if has_code:
        return "python", None
    if has_command:
        return "shell", None
    return None, "Provide `code` for Python mode or `command` for shell mode."


async def _exec_shell_command(
    sandbox: object,
    *,
    command: str,
    cwd: str | None,
    timeout: int,
) -> dict[str, object]:
    wrapped_command = command if not cwd else f"cd {shlex.quote(cwd)} && {command}"
    response = await sandbox.process.exec(_wrap_bash_command(wrapped_command), timeout=timeout)
    stdout = getattr(response, "result", "") or ""
    fallback_exit_code = getattr(response, "exit_code", None)
    cleaned_stdout, exit_code = _extract_exit_code(stdout, fallback_exit_code=fallback_exit_code)
    return {
        "command": command,
        "stdout": cleaned_stdout,
        "stderr": cleaned_stdout if exit_code != 0 else "",
        "exit_code": exit_code,
    }


async def _run_shell_with_recovery(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    command: str,
    cwd: str | None,
    timeout: int,
) -> tuple[dict[str, object] | None, object, ToolResult | None]:
    try:
        return await _exec_shell_command(sandbox, command=command, cwd=cwd, timeout=timeout), sandbox, None
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            return await _exec_shell_command(sandbox, command=command, cwd=cwd, timeout=timeout), sandbox, None
        except Exception as recovery_exc:
            return None, sandbox, ToolResult(output=f"Execution failed: {recovery_exc}", is_error=True)


async def _resolve_repo_root(
    context: ToolExecutionContext,
    sandbox: object,
) -> tuple[str | None, object, ToolResult | None]:
    repo_cwd = _get_cwd(context)
    if not repo_cwd:
        return None, sandbox, ToolResult(
            output=(
                "daytona_codeact: coordinated transactional execution requires an injected repo cwd."
            ),
            is_error=True,
        )
    shell_result, sandbox, tool_error = await _run_shell_with_recovery(
        context,
        sandbox,
        command="git rev-parse --show-toplevel",
        cwd=repo_cwd,
        timeout=60,
    )
    if tool_error is not None:
        return None, sandbox, tool_error
    assert shell_result is not None
    repo_root = str(shell_result.get("stdout", "") or "").strip()
    if int(shell_result.get("exit_code", 1)) != 0 or not repo_root:
        return None, sandbox, ToolResult(
            output=(
                "daytona_codeact: coordinated transactional execution requires a git workspace root."
            ),
            is_error=True,
        )
    return repo_root, sandbox, None


def _build_tool_output(
    *,
    context: ToolExecutionContext,
    status: str,
    files_written: int,
    shells: list[dict[str, object]],
    script_stdout: str,
    write_errors: list[str],
    write_conflicts: list[str],
    warnings: list[str],
    error: str = "",
) -> ToolResult:
    shell_summaries: list[str] = []
    shell_outputs: list[dict[str, object]] = []
    for shell_result in shells[:3]:
        command = str(shell_result.get("command", "") or "")
        exit_code = shell_result.get("exit_code", "?")
        shell_summaries.append(f"$ {command[:80]} -> exit {exit_code}")
        shell_outputs.append(
            {
                "command": command,
                "exit_code": exit_code,
                "stdout": str(shell_result.get("stdout", "") or ""),
                "stderr": str(shell_result.get("stderr", "") or ""),
            }
        )

    return ToolResult(
        output=json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "status": status,
                "files_written": files_written,
                "shells_run": len(shells),
                "shell_summaries": shell_summaries,
                "shell_outputs": shell_outputs,
                "script_stdout": script_stdout,
                "write_errors": write_errors,
                "write_conflicts": write_conflicts,
                "warnings": warnings,
                "error": error[:500] if error else "",
            }
        ),
        is_error=(status == "error" or bool(write_errors)),
        metadata={
            "status": status,
            "files_written": files_written,
            "shells_run": len(shells),
            "conflict": bool(write_conflicts),
        },
    )


async def _execute_python_wrapper(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    code: str,
    cwd: str | None,
    enforce_team_shell_policy: bool,
) -> tuple[str | None, object, ToolResult | None]:
    run_id = uuid.uuid4().hex[:8]
    wrapper = _build_wrapper(
        code,
        run_id=run_id,
        cwd=cwd,
        enforce_team_shell_policy=enforce_team_shell_policy,
    )
    script_path = f"/tmp/codeact-wrapper-{run_id}.py"
    exec_command = _build_exec_command(script_path, cwd=cwd)
    try:
        await _upload_file_compat(sandbox, wrapper.encode("utf-8"), script_path)
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            await _upload_file_compat(sandbox, wrapper.encode("utf-8"), script_path)
        except Exception as recovery_exc:
            return None, sandbox, ToolResult(
                output=f"Failed to upload script: {recovery_exc}",
                is_error=True,
            )

    try:
        response = await sandbox.process.exec(exec_command, timeout=900)
        return getattr(response, "result", "") or "", sandbox, None
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            response = await sandbox.process.exec(exec_command, timeout=900)
            return getattr(response, "result", "") or "", sandbox, None
        except Exception as recovery_exc:
            return None, sandbox, ToolResult(
                output=f"Execution failed: {recovery_exc}",
                is_error=True,
            )


def _shell_error_result(message: str) -> ToolResult:
    return ToolResult(output=message, is_error=True)


def _shell_result_error_detail(shell_result: dict[str, object]) -> str:
    return str(shell_result.get("stderr", "") or shell_result.get("stdout", "") or "")


async def _open_transaction(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    enabled: bool,
) -> tuple[object | None, object, ToolResult | None]:
    if not enabled:
        return None, sandbox, None
    repo_root, sandbox, root_error = await _resolve_repo_root(context, sandbox)
    if root_error is not None:
        return None, sandbox, root_error
    assert repo_root is not None
    try:
        return await create_codeact_transaction(sandbox, repo_root), sandbox, None
    except Exception as exc:
        return None, sandbox, ToolResult(
            output=f"Failed to create codeact transaction: {exc}",
            is_error=True,
        )


async def _commit_transaction(
    context: ToolExecutionContext,
    sandbox: object,
    tx: object | None,
) -> tuple[int, list[str], list[str], list[str]]:
    if tx is None:
        return 0, [], [], []
    changes = await collect_transaction_changes(sandbox, tx)
    report = await commit_transaction_changes(context, tx, changes)
    return (
        len(report.committed),
        [result.message or result.path for result in report.errors],
        [str(tx.repo_root + "/" + result.path) for result in report.conflicts],
        list(report.warnings),
    )


@tool(
    name="daytona_codeact",
    description=(
        "Execute either Python code or a direct shell command in the Daytona sandbox. "
        "Use `command` for tests, builds, and verification; use `code` for multi-step "
        "Python with read()/write()/shell() helpers."
    ),
    short_description="Run shell commands or Python in the sandbox.",
    background="optional",
)
async def daytona_codeact(
    mode: Literal["python", "shell"] | None = None,
    code: str | None = None,
    command: str | None = None,
    timeout: int = 900,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Execute shell commands or Python code in the Daytona sandbox.

    Args:
        mode: Optional execution mode. When omitted, `code` implies Python mode and
            `command` implies shell mode.
        code: Python code to execute. Has access to read(path), write(path, content),
            and shell(command, timeout=900).
        command: Shell command to execute directly.
        timeout: Timeout in seconds for shell mode execution.

    Returns:
        status (str): Execution status — ok or error
        files_written (int): Number of files committed or written
        shells_run (int): Number of shell commands executed
        error (str): Error message if failed
    """
    resolved_mode, mode_error = _resolve_mode(mode=mode, code=code, command=command)
    if mode_error is not None:
        return ToolResult(output=mode_error, is_error=True)

    assert resolved_mode is not None

    if resolved_mode == "shell":
        direct_command = command or ""
        destructive_error = _destructive_git_command_error(direct_command)
        if destructive_error is None:
            destructive_error = destructive_shell_command_error(direct_command)
        if destructive_error is not None:
            return _shell_error_result(destructive_error)
        if is_coordinated_team_agent(context) and "2>&1" in direct_command:
            return _shell_error_result(
                "CodeAct policy error: do not append `2>&1`; stdout/stderr are already captured."
            )

    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)

    repo_cwd = _get_cwd(context)

    if resolved_mode == "shell":
        tx, sandbox, tx_error = await _open_transaction(
            context,
            sandbox,
            enabled=is_coordinated_team_agent(context),
        )
        if tx_error is not None:
            return tx_error
        try:
            shell_result, sandbox, tool_error = await _run_shell_with_recovery(
                context,
                sandbox,
                command=command or "",
                cwd=tx.scratch_root if tx is not None else repo_cwd,
                timeout=timeout,
            )
            if tool_error is not None:
                return tool_error
            assert shell_result is not None
            exit_code = int(shell_result.get("exit_code", 1))
            files_written = 0
            write_errors: list[str] = []
            write_conflicts: list[str] = []
            warnings: list[str] = []
            if exit_code == 0:
                (
                    files_written,
                    write_errors,
                    write_conflicts,
                    warnings,
                ) = await _commit_transaction(context, sandbox, tx)
            return _build_tool_output(
                context=context,
                status="ok" if exit_code == 0 and not write_errors else "error",
                files_written=files_written,
                shells=[shell_result],
                script_stdout="",
                write_errors=write_errors,
                write_conflicts=write_conflicts,
                warnings=warnings,
                error=_shell_result_error_detail(shell_result) if exit_code != 0 else "",
            )
        finally:
            if tx is not None:
                await cleanup_codeact_transaction(sandbox, tx)

    tx, sandbox, tx_error = await _open_transaction(
        context,
        sandbox,
        enabled=is_coordinated_team_agent(context),
    )
    if tx_error is not None:
        return tx_error

    try:
        stdout, sandbox, tool_error = await _execute_python_wrapper(
            context,
            sandbox,
            code=code or "",
            cwd=tx.scratch_root if tx is not None else repo_cwd,
            enforce_team_shell_policy=is_coordinated_team_agent(context),
        )
        if tool_error is not None:
            return tool_error
        assert stdout is not None

        stdout, _ = _extract_exit_code(stdout, fallback_exit_code=0)
        stdout_lines = stdout.splitlines()
        script_stdout = "\n".join(stdout_lines[:-1]).strip() if stdout_lines else ""
        try:
            result_line = stdout_lines[-1] if stdout_lines else "{}"
            result = json.loads(result_line)
        except (json.JSONDecodeError, IndexError):
            return ToolResult(
                output=f"Script output:\n{stdout[:4000]}",
                metadata={"status": "unknown"},
            )

        manifest_path = str(result.get("manifest", "") or "")
        if not manifest_path:
            if result.get("status") == "error":
                return ToolResult(
                    output=f"CodeAct execution error:\n{stdout[:4000]}",
                    is_error=True,
                )
            return ToolResult(output=f"Script output:\n{stdout[:4000]}")

        try:
            raw = await sandbox.fs.download_file(manifest_path)
            manifest = json.loads(raw.decode("utf-8") if isinstance(raw, bytes) else raw)
        except Exception:
            if result.get("status") == "error":
                return ToolResult(
                    output=_format_codeact_error(stdout=stdout),
                    is_error=True,
                )
            return ToolResult(output=f"Script completed but manifest unreadable:\n{stdout[:4000]}")

        shells = list(manifest.get("shells", []) or [])
        if result.get("status") == "error":
            manifest_error = str(manifest.get("error", "") or "")
            return ToolResult(
                output=_format_codeact_error(stdout=stdout, manifest_error=manifest_error),
                is_error=True,
                metadata={
                    "status": manifest.get("status", "error"),
                    "shells_run": len(shells),
                },
            )

        files_written = len(_coalesce_manifest_writes(list(manifest.get("writes", []) or [])))
        write_errors: list[str] = []
        write_conflicts: list[str] = []
        warnings: list[str] = []

        if tx is not None:
            (
                files_written,
                write_errors,
                write_conflicts,
                warnings,
            ) = await _commit_transaction(context, sandbox, tx)

        return _build_tool_output(
            context=context,
            status="ok" if not write_errors else "error",
            files_written=files_written,
            shells=shells,
            script_stdout=script_stdout,
            write_errors=write_errors,
            write_conflicts=write_conflicts,
            warnings=warnings,
            error=str(manifest.get("error", "") or ""),
        )
    finally:
        if tx is not None:
            await cleanup_codeact_transaction(sandbox, tx)


daytona_codeact.input_model = DaytonaCodeActInput

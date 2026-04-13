"""CodeAct tool — multi-step code thinking and execution in a sandbox.

Executes a Python script in the sandbox with staged file I/O.
The script has access to read(), write(), and shell() helpers. Helper-based
writes are committed after the script finishes.
"""

from __future__ import annotations

import base64
import json
import logging
import uuid
from collections import OrderedDict

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    abort_ci_write,
    finalize_ci_write,
    prepare_ci_edit_intent,
    prepare_declared_shell_outputs,
    prepare_ci_write,
    release_declared_shell_outputs,
    release_ci_edit_intent,
    sync_write_to_ci,
)
from tools.daytona_toolkit.ci_integration import command_may_mutate_workspace, sync_shell_mutations
from tools.daytona_toolkit.tools import (
    _get_cwd,
    _recover_sandbox,
    _require_sandbox,
    _resolve_path,
    _team_repo_write_warning,
    _upload_file_compat,
    _wrap_bash_command,
    record_coordination_warning,
)
from tools.core.decorator import tool

logger = logging.getLogger(__name__)

_WRAPPER_TEMPLATE = r'''
import base64, hashlib, json, os, re, subprocess, sys, traceback

_RUN_ID = "{run_id}"
_MANIFEST = {{"reads": [], "writes": [], "shells": [], "status": "ok", "error": ""}}
_CODEACT_CWD = {codeact_cwd}
_REQUIRE_DECLARED_SHELL_OUTPUTS = {require_declared_shell_outputs}
_DECLARED_OUTPUT_PATHS = {declared_output_paths}
_SHELL_MUTATION_PATTERN = re.compile(
    r"(^|[;&|]\s*)("
    r"cat\s+>|tee\s|cp\s|mv\s|rm\s|touch\s|mkdir\s|install\s|ln\s|"
    r"git\s+(apply|checkout|restore|reset|clean|mv|rm)\b|"
    r"sed\s+-i\b|perl\s+-pi\b|patch\b|ed\b|ex\b|"
    r".*>>|.*[^<]>(?!&)[^>]"
    r")",
    flags=re.IGNORECASE,
)
_READ_ONLY_TEST_COMMAND_PATTERN = re.compile(
    r"^\s*(?:python(?:\d+(?:\.\d+)*)?\s+-m\s+)?(?:pytest|py\.test)\b",
    flags=re.IGNORECASE,
)
_USER_LOCAL_BIN_EXPORT = 'export PATH="$HOME/.local/bin:$PATH"'
_PROJECT_VENV_BIN_EXPORT = 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi'
_PYTHON3_SHIM = 'if command -v python3 >/dev/null 2>&1; then python() {{ command python3 "$@"; }}; fi'

def _command_may_mutate_workspace(command):
    stripped = (command or "").strip()
    if not stripped:
        return False
    if _READ_ONLY_TEST_COMMAND_PATTERN.match(stripped):
        return False
    return bool(_SHELL_MUTATION_PATTERN.search(stripped))

def read(path):
    """Read a file and track the read."""
    with open(path, "r") as f:
        content = f.read()
    h = hashlib.sha256(content.encode()).hexdigest()[:16]
    _MANIFEST["reads"].append({{"path": path, "hash": h}})
    return content

def write(path, content):
    """Stage a file write (not written to disk until commit)."""
    _MANIFEST["writes"].append({{"path": path, "content": content}})

def shell(command, timeout=900):
    """Execute a shell command."""
    if (
        _REQUIRE_DECLARED_SHELL_OUTPUTS
        and _command_may_mutate_workspace(command)
        and not _DECLARED_OUTPUT_PATHS
    ):
        message = (
            "Mutating shell calls must declare `declared_output_paths` in team "
            "coordination mode. Prefer daytona_write_file/daytona_edit_file, or list every "
            "path the command may create, modify, move, or delete before running it."
        )
        _MANIFEST["shells"].append(
            {{
                "command": command,
                "stdout": "",
                "stderr": message,
                "exit_code": -1,
                "declared_output_paths": _DECLARED_OUTPUT_PATHS,
                "blocked": True,
            }}
        )
        raise RuntimeError(message)
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
            "declared_output_paths": _DECLARED_OUTPUT_PATHS,
        }}
    except subprocess.TimeoutExpired:
        result = {{
            "command": command,
            "stdout": "",
            "stderr": "timeout",
            "exit_code": -1,
            "declared_output_paths": _DECLARED_OUTPUT_PATHS,
        }}
    except Exception as e:
        result = {{
            "command": command,
            "stdout": "",
            "stderr": str(e),
            "exit_code": -1,
            "declared_output_paths": _DECLARED_OUTPUT_PATHS,
        }}
    _MANIFEST["shells"].append(result)
    return result

try:
    _CODE = base64.b64decode("{code_b64}").decode("utf-8")
    exec(_CODE, {{"read": read, "write": write, "shell": shell, "__name__": "__codeact__"}})
except Exception as e:
    _MANIFEST["status"] = "error"
    _MANIFEST["error"] = traceback.format_exc()[:2000]

# Write manifest
with open("/tmp/codeact-{run_id}.json", "w") as f:
    json.dump(_MANIFEST, f)

print(json.dumps({{"manifest": "/tmp/codeact-{run_id}.json", "status": _MANIFEST["status"]}}))
'''


def _build_wrapper(
    code: str,
    *,
    run_id: str,
    cwd: str | None,
    require_declared_shell_outputs: bool = False,
    declared_output_paths: list[str] | None = None,
) -> str:
    code_b64 = base64.b64encode(code.encode("utf-8")).decode("ascii")
    return _WRAPPER_TEMPLATE.format(
        run_id=run_id,
        code_b64=code_b64,
        codeact_cwd=json.dumps(cwd) if cwd else "None",
        require_declared_shell_outputs="True" if require_declared_shell_outputs else "False",
        declared_output_paths=json.dumps(declared_output_paths or []),
    )


def _build_exec_command(script_path: str, *, cwd: str | None) -> str:
    command = f"python3 {script_path}"
    if cwd:
        command = f"cd {json.dumps(cwd)} && {command}"
    return _wrap_bash_command(command)


def _coalesce_staged_writes(writes: list[dict[str, object]]) -> list[tuple[str, str]]:
    """Keep the final staged content for each path in manifest order."""
    final_writes: OrderedDict[str, str] = OrderedDict()
    for item in writes:
        path = str(item.get("path", "") or "")
        if not path:
            continue
        content = str(item.get("content", "") or "")
        if path in final_writes:
            del final_writes[path]
        final_writes[path] = content
    return list(final_writes.items())


def _read_hashes_by_path(reads: list[dict[str, object]]) -> dict[str, str]:
    """Return the latest recorded read hash for each path."""
    hashes: dict[str, str] = {}
    for item in reads:
        path = str(item.get("path", "") or "")
        read_hash = str(item.get("hash", "") or "")
        if path and read_hash:
            hashes[path] = read_hash
    return hashes


async def _commit_staged_write(
    *,
    context: ToolExecutionContext,
    sandbox: object,
    path: str,
    content: str,
    expected_hash: str,
) -> tuple[bool, str | None, bool, str | None]:
    """Commit a helper-staged write with CI coordination when available."""
    prepared = None
    intent_id = None
    contract_warning = _team_repo_write_warning(
        context,
        path,
        tool_name="daytona_codeact.write",
    )
    if contract_warning is not None:
        record_coordination_warning(
            context,
            category="write_scope",
            message=contract_warning,
        )
    try:
        prepared, _, err = prepare_ci_write(
            context,
            path,
            expected_hash=expected_hash,
            allow_scope_drift=True,
        )
        if err is not None:
            return False, err, True, contract_warning

        if prepared is not None:
            prepared, intent_id = prepare_ci_edit_intent(context, prepared, content=content)
            result = finalize_ci_write(
                context,
                prepared,
                content=content,
                edit_type="codeact",
                description="daytona_codeact",
            )

            if getattr(result, "success", False):
                return True, None, False, contract_warning
            return (
                False,
                str(getattr(result, "message", "") or "Write failed"),
                bool(getattr(result, "conflict", False)),
                contract_warning,
            )

        await _upload_file_compat(sandbox, content.encode("utf-8"), path)
        sync_write_to_ci(
            context,
            path,
            content,
            edit_type="codeact",
            description="daytona_codeact",
        )
        return True, None, False, contract_warning
    except Exception as exc:
        return False, str(exc), False, contract_warning
    finally:
        if intent_id is not None:
            release_ci_edit_intent(context, intent_id)
        if prepared is not None:
            abort_ci_write(context, prepared)


@tool(
    name="daytona_codeact",
    description="Execute Python code with staged file I/O via read(), write(), and shell() helpers.",
    background="optional",
)
async def daytona_codeact(
    code: str,
    declared_output_paths: list[str] | None = None,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Execute multi-step code with staged file I/O in the Daytona sandbox.

    Args:
        code: Python code to execute in the sandbox. Has access to read(path), write(path, content), and shell(command, timeout=900). Helper-based writes are staged and committed after execution.

    Returns:
        status (str): Execution status — ok or error
        files_written (int): Number of files committed
        shells_run (int): Number of shell commands executed
        error (str): Error message if failed
    """
    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)

    run_id = uuid.uuid4().hex[:8]
    resolved_declared_output_paths = [
        _resolve_path(path, context)
        for path in (declared_output_paths or [])
        if isinstance(path, str) and path.strip()
    ]
    # Build and upload wrapper script
    repo_cwd = _get_cwd(context)
    if repo_cwd is None:
        logger.warning("daytona_codeact: no daytona_cwd set — shell() will use sandbox default cwd")
    wrapper = _build_wrapper(
        code,
        run_id=run_id,
        cwd=repo_cwd,
        require_declared_shell_outputs=bool(context.metadata.get("require_declared_shell_outputs")),
        declared_output_paths=resolved_declared_output_paths,
    )
    script_path = f"/tmp/codeact-wrapper-{run_id}.py"
    exec_command = _build_exec_command(script_path, cwd=repo_cwd)
    prepared_shell_outputs: list[object] = []
    shell_sync: dict[str, object] = {"enabled": False, "files": 0, "truncated": False}
    warnings: list[str] = []

    for path in resolved_declared_output_paths:
        contract_warning = _team_repo_write_warning(
            context,
            path,
            tool_name="daytona_codeact.declared_output",
        )
        if contract_warning is None:
            continue
        warnings.append(contract_warning)
        record_coordination_warning(
            context,
            category="write_scope",
            message=contract_warning,
        )

    if resolved_declared_output_paths:
        prepared_shell_outputs, scope_packet, err = prepare_declared_shell_outputs(
            context,
            declared_output_paths=resolved_declared_output_paths,
        )
        if err is not None:
            return ToolResult(
                output=err,
                is_error=True,
                metadata={"scope_packet": scope_packet, "conflict": True},
            )

    try:
        try:
            await _upload_file_compat(sandbox, wrapper.encode("utf-8"), script_path)
        except Exception as exc:
            try:
                sandbox = await _recover_sandbox(context, exc)
                await _upload_file_compat(sandbox, wrapper.encode("utf-8"), script_path)
            except Exception as recovery_exc:
                return ToolResult(output=f"Failed to upload script: {recovery_exc}", is_error=True)

        # Execute
        try:
            response = await sandbox.process.exec(
                exec_command,
                timeout=900,
            )
            stdout = response.result or ""
        except Exception as exc:
            try:
                sandbox = await _recover_sandbox(context, exc)
                response = await sandbox.process.exec(
                    exec_command,
                    timeout=900,
                )
                stdout = response.result or ""
            except Exception as recovery_exc:
                return ToolResult(output=f"Execution failed: {recovery_exc}", is_error=True)

        # Parse output
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

        # Read manifest
        manifest_path = result.get("manifest", "")
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
                    output=f"CodeAct execution error:\n{stdout[:4000]}",
                    is_error=True,
                )
            return ToolResult(output=f"Script completed but manifest unreadable:\n{stdout[:4000]}")

        shells = manifest.get("shells", [])
        mutating_shell_commands = [
            str(sh.get("command", "") or "")
            for sh in shells
            if not bool(sh.get("blocked"))
            and command_may_mutate_workspace(str(sh.get("command", "") or ""))
        ]
        if mutating_shell_commands:
            shell_sync = await sync_shell_mutations(
                context,
                command=" && ".join(mutating_shell_commands),
                declared_output_paths=resolved_declared_output_paths or None,
            )
            sync_error = str(shell_sync.get("error", "") or "")
            if sync_error:
                warnings.append(sync_error)

        if result.get("status") == "error":
            return ToolResult(
                output=f"CodeAct execution error:\n{stdout[:4000]}",
                is_error=True,
                metadata={
                    "status": manifest.get("status", "error"),
                    "shells_run": len(shells),
                    "shell_ci_sync": shell_sync,
                },
            )

        # Commit staged writes
        writes = manifest.get("writes", [])
        read_hashes = _read_hashes_by_path(manifest.get("reads", []))
        committed = 0
        errors = []
        conflicts = []

        for path, content in _coalesce_staged_writes(writes):
            ok, error, conflict, contract_warning = await _commit_staged_write(
                context=context,
                sandbox=sandbox,
                path=path,
                content=content,
                expected_hash=read_hashes.get(path, ""),
            )
            if contract_warning is not None:
                warnings.append(contract_warning)
            if ok:
                committed += 1
                continue
            if conflict:
                conflicts.append(path)
            errors.append(f"{path}: {error or 'Write failed'}")

        # Build output
        shell_summaries = []
        shell_outputs = []
        for sh in shells[:3]:
            cmd = sh.get("command", "")[:80]
            exit_code = sh.get("exit_code", "?")
            shell_summaries.append(f"$ {cmd} → exit {exit_code}")
            shell_outputs.append(
                {
                    "command": sh.get("command", ""),
                    "exit_code": exit_code,
                    "stdout": sh.get("stdout", ""),
                    "stderr": sh.get("stderr", ""),
                }
            )

        output = json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "status": manifest.get("status", "unknown"),
                "files_written": committed,
                "shells_run": len(shells),
                "shell_summaries": shell_summaries,
                "shell_outputs": shell_outputs,
                "script_stdout": script_stdout,
                "write_errors": errors or [],
                "write_conflicts": conflicts,
                "warnings": warnings,
                "error": manifest.get("error", "")[:500] if manifest.get("error") else "",
                "shell_ci_sync": shell_sync,
            }
        )

        return ToolResult(
            output=output,
            is_error=bool(errors),
            metadata={
                "status": manifest.get("status", "unknown"),
                "files_written": committed,
                "shells_run": len(shells),
                "conflict": bool(conflicts),
                "shell_ci_sync": shell_sync,
            },
        )
    finally:
        release_declared_shell_outputs(context, prepared_shell_outputs)

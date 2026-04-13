"""CodeAct tool — multi-step code thinking and execution in a sandbox.

Executes a Python script in the sandbox with staged file I/O.
The script has access to read(), write(), and shell() helpers. Helper-based
writes are committed after the script finishes.
"""

from __future__ import annotations

import ast
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
from tools.daytona_toolkit.ci_integration import (
    command_may_mutate_workspace,
    detect_workspace_regression,
    snapshot_dirty_files,
    sync_shell_mutations,
)
from tools.daytona_toolkit._daytona_utils import _extract_exit_code, is_coordinated_team_agent
from tools.daytona_toolkit.tools import (
    _get_cwd,
    _recover_sandbox,
    _require_sandbox,
    _resolve_path,
    _team_repo_write_error,
    _team_repo_write_warning,
    _upload_file_compat,
    _wrap_bash_command,
    record_coordination_warning,
)
from tools.core.decorator import tool

logger = logging.getLogger(__name__)

_BLOCKED_CODEACT_MODULES = frozenset({"subprocess", "shutil"})
_BLOCKED_CODEACT_CALLS = frozenset(
    {
        "subprocess.run",
        "subprocess.Popen",
        "subprocess.call",
        "subprocess.check_call",
        "subprocess.check_output",
        "os.system",
        "os.popen",
    }
)


def _dotted_name(node: ast.AST) -> str:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        base = _dotted_name(node.value)
        return f"{base}.{node.attr}" if base else node.attr
    return ""


def _detect_blocked_codeact_usage(code: str) -> list[str]:
    """Return shell-policy violations in *code* before sandbox execution."""
    try:
        tree = ast.parse(code)
    except SyntaxError:
        return []

    violations: list[str] = []
    seen: set[str] = set()

    def _note(message: str) -> None:
        if message not in seen:
            seen.add(message)
            violations.append(message)

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".", 1)[0]
                if root in _BLOCKED_CODEACT_MODULES:
                    _note(f"import {root}")
        elif isinstance(node, ast.ImportFrom):
            root = (node.module or "").split(".", 1)[0]
            if root in _BLOCKED_CODEACT_MODULES:
                _note(f"from {root} import ...")
        elif isinstance(node, ast.Call):
            func_name = _dotted_name(node.func)
            if func_name in _BLOCKED_CODEACT_CALLS:
                _note(f"{func_name}(...)")
                continue
            if func_name in {"__import__", "builtins.__import__", "importlib.import_module"}:
                if node.args and isinstance(node.args[0], ast.Constant):
                    value = node.args[0].value
                    if isinstance(value, str) and value.split(".", 1)[0] in _BLOCKED_CODEACT_MODULES:
                        _note(f"{func_name}({value!r})")
    return violations


def _codeact_shell_policy_error(violations: list[str]) -> ToolResult:
    preview = ", ".join(violations[:3])
    if len(violations) > 3:
        preview += ", ..."
    return ToolResult(
        output=(
            "CodeAct policy error: coordinated team lanes must use `shell(\"...\")` "
            "for repo commands inside `daytona_codeact`. "
            f"Blocked pattern(s): {preview}. "
            "Replace subprocess/os process wrappers with `shell(...)`."
        ),
        is_error=True,
        metadata={"status": "blocked_shell_policy"},
    )


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
            "Use `shell(\"...\")` for commands inside `daytona_codeact`; "
            "do not import `subprocess` or call `os.system()`."
        )
    return "\n".join(lines)

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
    r"git\s+(apply|checkout|restore|reset|clean|stash|merge|rebase|cherry-pick|mv|rm)\b|"
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

def shell(command, timeout=900):
    """Execute a shell command."""
    # Hard block: destructive git commands that destroy the shared workspace.
    # These bypass OCC and undo other agents' work. Always blocked — cannot
    # be overridden with declared_output_paths or coordination-mode flags.
    if _DESTRUCTIVE_GIT_PATTERN.search(command or ""):
        message = (
            "BLOCKED: destructive git commands (stash, reset --hard, checkout --, clean) "
            "are forbidden in team coordination mode. They destroy other agents' work "
            "and bypass OCC. Use daytona_edit_file to revert specific edits instead."
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
    # Hard block: destructive shell commands that move/remove workspace roots,
    # system directories, or recursively destroy broad path trees.
    if _DESTRUCTIVE_SHELL_PATTERN.search(command or ""):
        message = (
            "BLOCKED: destructive shell command that targets workspace or system "
            "directories (rm -r /testbed, mv /testbed, etc.) is forbidden. "
            "These commands destroy the shared workspace and cannot be undone. "
            "Use targeted file operations instead."
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

import builtins as _builtins_mod
_BLOCKED_MODULES = frozenset({{"subprocess", "shutil"}})
_real_import = _builtins_mod.__import__

def _guarded_import(name, *args, **kwargs):
    top = name.split(".")[0]
    if top in _BLOCKED_MODULES:
        raise ImportError(
            f"import {{name!r}} is blocked in codeact. "
            "Use shell() for commands and read()/write() for file I/O."
        )
    return _real_import(name, *args, **kwargs)

_sandbox_builtins = dict(vars(_builtins_mod))
_sandbox_builtins["__import__"] = _guarded_import

try:
    _CODE = base64.b64decode("{code_b64}").decode("utf-8")
    exec(_CODE, {{"read": read, "write": write, "shell": shell, "__name__": "__codeact__", "__builtins__": _sandbox_builtins}})
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
    contract_error = _team_repo_write_error(
        context,
        path,
        tool_name="daytona_codeact.write",
    )
    if contract_error is not None:
        return False, contract_error, False, None
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
    if is_coordinated_team_agent(context):
        violations = _detect_blocked_codeact_usage(code)
        if violations:
            return _codeact_shell_policy_error(violations)

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
        require_declared_shell_outputs=(
            bool(context.metadata.get("require_declared_shell_outputs"))
            or is_coordinated_team_agent(context)
        ),
        declared_output_paths=resolved_declared_output_paths,
    )
    script_path = f"/tmp/codeact-wrapper-{run_id}.py"
    exec_command = _build_exec_command(script_path, cwd=repo_cwd)
    prepared_shell_outputs: list[object] = []
    shell_sync: dict[str, object] = {"enabled": False, "files": 0, "truncated": False}
    shell_write_errors: list[str] = []
    warnings: list[str] = []

    for path in resolved_declared_output_paths:
        contract_error = _team_repo_write_error(
            context,
            path,
            tool_name="daytona_codeact.declared_output",
        )
        if contract_error is not None:
            return ToolResult(output=contract_error, is_error=True)
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

    # Layer 2: snapshot dirty files before execution so we can detect
    # regressions (e.g. git stash wiping the working tree).
    pre_dirty_snapshot = await snapshot_dirty_files(context)

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

        # Strip the __CODEX_EXIT_CODE__ marker appended by _wrap_bash_command
        # so the last line of stdout is the JSON manifest line, not the marker.
        stdout, _ = _extract_exit_code(stdout, fallback_exit_code=0)

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
                    output=_format_codeact_error(stdout=stdout),
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
            shell_write_errors.extend(
                str(item)
                for item in (shell_sync.get("write_errors") or [])
                if str(item).strip()
            )
            warnings.extend(
                str(item)
                for item in (shell_sync.get("write_warnings") or [])
                if str(item).strip()
            )
            sync_error = str(shell_sync.get("error", "") or "")
            if sync_error:
                if bool(shell_sync.get("missing_declarations")):
                    shell_write_errors.append(sync_error)
                else:
                    warnings.append(sync_error)

        # Layer 2: detect workspace regressions — files that were dirty
        # before execution but are now clean (reverted by destructive
        # commands like git stash, git checkout, etc.).
        if shells:
            regressed = await detect_workspace_regression(
                context, pre_snapshot=pre_dirty_snapshot,
            )
            if regressed:
                sample = ", ".join(regressed[:10])
                suffix = f" (and {len(regressed) - 10} more)" if len(regressed) > 10 else ""
                warnings.append(
                    f"WORKSPACE REGRESSION: {len(regressed)} file(s) were silently "
                    f"reverted by a shell command: {sample}{suffix}. "
                    f"Other agents' work may have been lost."
                )

        if result.get("status") == "error":
            manifest_error = str(manifest.get("error", "") or "")
            return ToolResult(
                output=_format_codeact_error(
                    stdout=stdout,
                    manifest_error=manifest_error,
                ),
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
        errors.extend(shell_write_errors)

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

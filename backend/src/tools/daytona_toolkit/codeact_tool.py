"""CodeAct tool — multi-step code thinking and execution in a sandbox.

Executes a Python script in the sandbox with atomic file I/O.
The script has access to read(), write(), and shell() helpers. All writes
are staged and committed atomically after the script finishes.
"""

from __future__ import annotations

import ast
import base64
import json
import logging
import re
import uuid
from typing import Any

from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit.tools import (
    _get_cwd,
    _recover_sandbox,
    _require_sandbox,
    _verification_surface_enforcement_mode,
    _wrap_bash_command,
)
from tools.daytona_toolkit.ci_integration import (
    prime_cache_after_write,
    record_edit_in_ledger,
)
from tools.core.decorator import tool

logger = logging.getLogger(__name__)

_TEAM_CONTRACT_AGENT_NAMES = frozenset({"developer", "validator"})
_VERIFY_PATH_RE = re.compile(r"(?<![A-Za-z0-9_./-])([A-Za-z0-9_./-]+\.py)(?![A-Za-z0-9_./-])")
_DISALLOWED_RUNTIME_CALLS = frozenset(
    {
        "asyncio.create_subprocess_exec",
        "asyncio.create_subprocess_shell",
        "os.popen",
        "os.system",
        "subprocess.Popen",
        "subprocess.call",
        "subprocess.check_call",
        "subprocess.check_output",
        "subprocess.getoutput",
        "subprocess.getstatusoutput",
        "subprocess.run",
    }
)
_AMBIENT_INSTALL_PATTERNS = (
    re.compile(r"(^|[;&|]\s*)(python(?:3)?\s+-m\s+pip|pip3?|uv\s+pip)\s+install\b"),
    re.compile(r"(^|[;&|]\s*)(poetry|conda|mamba|micromamba)\s+install\b"),
    re.compile(r"(^|[;&|]\s*)(apt|apt-get|apk|brew|dnf|yum)\s+install\b"),
)

_WRAPPER_TEMPLATE = r'''
import base64, hashlib, json, os, subprocess, sys, traceback

_RUN_ID = "{run_id}"
_MANIFEST = {{"reads": [], "writes": [], "shells": [], "status": "ok", "error": ""}}
_CODEACT_CWD = {codeact_cwd}

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

def shell(command, timeout=300):
    """Execute a shell command."""
    try:
        proc = subprocess.run(
            ["env", "-u", "LC_ALL", "bash", "-o", "pipefail", "-lc", command],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=_CODEACT_CWD or None,
        )
        result = {{"command": command, "stdout": proc.stdout, "stderr": proc.stderr, "exit_code": proc.returncode}}
    except subprocess.TimeoutExpired:
        result = {{"command": command, "stdout": "", "stderr": "timeout", "exit_code": -1}}
    except Exception as e:
        result = {{"command": command, "stdout": "", "stderr": str(e), "exit_code": -1}}
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


def _build_wrapper(code: str, *, run_id: str, cwd: str | None) -> str:
    code_b64 = base64.b64encode(code.encode("utf-8")).decode("ascii")
    return _WRAPPER_TEMPLATE.format(
        run_id=run_id,
        code_b64=code_b64,
        codeact_cwd=json.dumps(cwd) if cwd else "None",
    )


def _build_exec_command(script_path: str, *, cwd: str | None) -> str:
    command = f"python3 {script_path}"
    if cwd:
        command = f"cd {json.dumps(cwd)} && {command}"
    return _wrap_bash_command(command)


def _normalize_repo_relative_path(path: Any, repo_root: str) -> str | None:
    if not isinstance(path, str):
        return None
    cleaned = path.strip().replace("\\", "/")
    if not cleaned:
        return None
    while cleaned.startswith("./"):
        cleaned = cleaned[2:]
    cleaned = cleaned.rstrip("/")
    if not cleaned:
        return None
    if not cleaned.startswith("/"):
        return cleaned
    root = repo_root.rstrip("/")
    if root and cleaned.startswith(root + "/"):
        rel = cleaned[len(root) + 1 :].strip().rstrip("/")
        return rel or None
    return None


def _normalize_string_list(value: Any, repo_root: str) -> list[str]:
    if isinstance(value, str):
        values = [value]
    elif isinstance(value, list):
        values = [item for item in value if isinstance(item, str)]
    else:
        return []
    out: list[str] = []
    for item in values:
        normalized = _normalize_repo_relative_path(item, repo_root)
        if normalized:
            out.append(normalized)
    return out


def _extract_verify_paths(value: Any, repo_root: str) -> list[str]:
    if isinstance(value, str):
        candidates = [value]
    elif isinstance(value, list):
        candidates = [item for item in value if isinstance(item, str)]
    else:
        return []
    out: list[str] = []
    for item in candidates:
        stripped = item.strip()
        if not stripped:
            continue
        if stripped.endswith(".py") or "::" in stripped:
            normalized = _normalize_repo_relative_path(stripped.split("::", 1)[0], repo_root)
            if normalized:
                out.append(normalized)
        for match in _VERIFY_PATH_RE.findall(stripped):
            normalized = _normalize_repo_relative_path(match.split("::", 1)[0], repo_root)
            if normalized:
                out.append(normalized)
    return out


def _verification_surface_warning_paths(
    write_paths: list[str],
    *,
    allowed_write_paths: set[str],
    verify_paths: set[str],
) -> list[str]:
    return sorted(
        path
        for path in set(write_paths)
        if path in verify_paths and path not in allowed_write_paths
    )


def _resolve_call_name(node: ast.AST, aliases: dict[str, str]) -> str | None:
    parts: list[str] = []
    current = node
    while isinstance(current, ast.Attribute):
        parts.append(current.attr)
        current = current.value
    if not isinstance(current, ast.Name):
        return None
    parts.append(current.id)
    raw_name = ".".join(reversed(parts))
    root, *rest = raw_name.split(".")
    mapped_root = aliases.get(root, root)
    return ".".join([mapped_root, *rest]) if rest else mapped_root


def _detect_disallowed_runtime_calls(code: str) -> list[str]:
    try:
        tree = ast.parse(code)
    except SyntaxError:
        return []

    aliases: dict[str, str] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                name = alias.asname or alias.name
                aliases[name] = alias.name
        elif isinstance(node, ast.ImportFrom) and node.module:
            for alias in node.names:
                name = alias.asname or alias.name
                aliases[name] = f"{node.module}.{alias.name}"

    offenders: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        resolved = _resolve_call_name(node.func, aliases)
        if resolved in _DISALLOWED_RUNTIME_CALLS:
            offenders.add(resolved)
    return sorted(offenders)


def _team_codeact_contract(context: ToolExecutionContext) -> dict[str, Any] | None:
    agent_name = str(context.metadata.get("agent_name") or "").strip()
    if agent_name not in _TEAM_CONTRACT_AGENT_NAMES:
        return None
    if str(context.metadata.get("coordination_mode") or "").strip() != "ultra":
        return None
    repo_root = str(_get_cwd(context) or "")
    owned_files = set(_normalize_string_list(context.metadata.get("owned_files"), repo_root))
    touches_paths = set(_normalize_string_list(context.metadata.get("touches_paths"), repo_root))
    verify_paths = set(_extract_verify_paths(context.metadata.get("verify"), repo_root))
    verify_paths.update(_extract_verify_paths(context.metadata.get("owned_failures"), repo_root))
    return {
        "agent_name": agent_name,
        "repo_root": repo_root,
        "owned_files": owned_files,
        "touches_paths": touches_paths,
        "verify_paths": verify_paths,
        "verification_surface_write_enforcement": _verification_surface_enforcement_mode(context),
    }


def _team_codeact_preflight_error(code: str, contract: dict[str, Any] | None) -> str | None:
    if contract is None:
        return None
    offenders = _detect_disallowed_runtime_calls(code)
    if not offenders:
        return None
    rendered = ", ".join(offenders)
    return (
        "daytona_codeact: coordinated team developer/validator lanes must execute repo commands "
        "through the provided `shell(\"...\")` helper, not raw Python process APIs. "
        f"Found disallowed call(s): {rendered}."
    )


def _ambient_install_commands(shells: list[dict[str, Any]]) -> list[str]:
    offenders: list[str] = []
    for shell_call in shells:
        command = str(shell_call.get("command") or "").strip()
        if not command:
            continue
        if any(pattern.search(command) for pattern in _AMBIENT_INSTALL_PATTERNS):
            offenders.append(command)
    return offenders


def _team_codeact_manifest_error(
    manifest: dict[str, Any],
    contract: dict[str, Any] | None,
) -> str | None:
    if contract is None:
        return None

    shells = manifest.get("shells")
    if isinstance(shells, list):
        ambient_installs = _ambient_install_commands(
            [item for item in shells if isinstance(item, dict)]
        )
        if ambient_installs:
            rendered = "; ".join(ambient_installs[:2])
            return (
                "daytona_codeact: coordinated team developer/validator lanes must not mutate the "
                "ambient runtime environment with install commands. "
                f"Observed install command(s): {rendered}. "
                "Do not retry with pip/conda/uv install fallbacks; use one existing-runner probe, "
                "then continue diagnosis on owned repo files or surface ambient mismatch evidence."
            )

    writes = manifest.get("writes")
    if not isinstance(writes, list):
        return None

    repo_root = str(contract.get("repo_root") or "")
    write_paths = [
        rel
        for rel in (
            _normalize_repo_relative_path(item.get("path"), repo_root)
            for item in writes
            if isinstance(item, dict)
        )
        if rel
    ]
    if not write_paths:
        return None

    agent_name = str(contract.get("agent_name") or "")
    if agent_name == "validator":
        rendered = ", ".join(sorted(set(write_paths))[:3])
        return (
            "daytona_codeact: validator lanes must not write repository files. "
            f"Observed repo write(s): {rendered}."
        )

    allowed_write_paths = set(contract.get("owned_files") or ())
    allowed_write_paths.update(contract.get("touches_paths") or ())
    verify_paths = set(contract.get("verify_paths") or ())
    verify_writes = _verification_surface_warning_paths(
        write_paths,
        allowed_write_paths=allowed_write_paths,
        verify_paths=verify_paths,
    )
    if verify_writes:
        rendered = ", ".join(verify_writes[:3])
        message = (
            "daytona_codeact: developer lanes must keep verification surfaces read-only unless the "
            "WorkItem explicitly owns or widens to them. "
            f"Observed write(s) on verification paths: {rendered}."
        )
        if contract.get("verification_surface_write_enforcement") == "warn":
            logger.warning(message)
            return None
        return message
    return None


@tool(
    name="daytona_codeact",
    description="Execute Python code with atomic file I/O via read(), write(), and shell() helpers.",
    background="optional",
)
async def daytona_codeact(
    code: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Execute multi-step code with atomic file I/O in the Daytona sandbox.

    Args:
        code: Python code to execute in the sandbox. Has access to read(path), write(path, content), and shell(command, timeout=300). All writes are staged and committed atomically after execution.

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

    team_contract = _team_codeact_contract(context)
    preflight_error = _team_codeact_preflight_error(code, team_contract)
    if preflight_error is not None:
        return ToolResult(output=preflight_error, is_error=True)

    run_id = uuid.uuid4().hex[:8]
    # Build and upload wrapper script
    repo_cwd = _get_cwd(context)
    wrapper = _build_wrapper(code, run_id=run_id, cwd=repo_cwd)
    script_path = f"/tmp/codeact-wrapper-{run_id}.py"
    exec_command = _build_exec_command(script_path, cwd=repo_cwd)

    try:
        await sandbox.fs.upload_file(wrapper.encode("utf-8"), script_path)
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            await sandbox.fs.upload_file(wrapper.encode("utf-8"), script_path)
        except Exception as recovery_exc:
            return ToolResult(output=f"Failed to upload script: {recovery_exc}", is_error=True)

    # Execute
    try:
        response = await sandbox.process.exec(
            exec_command,
            timeout=300,
        )
        stdout = response.result or ""
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            response = await sandbox.process.exec(
                exec_command,
                timeout=300,
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

    if result.get("status") == "error":
        return ToolResult(
            output=f"CodeAct execution error:\n{stdout[:4000]}",
            is_error=True,
        )

    # Read manifest
    manifest_path = result.get("manifest", "")
    if not manifest_path:
        return ToolResult(output=f"Script output:\n{stdout[:4000]}")

    try:
        raw = await sandbox.fs.download_file(manifest_path)
        manifest = json.loads(raw.decode("utf-8") if isinstance(raw, bytes) else raw)
    except Exception:
        return ToolResult(output=f"Script completed but manifest unreadable:\n{stdout[:4000]}")

    manifest_error = _team_codeact_manifest_error(manifest, team_contract)
    if manifest_error is not None:
        return ToolResult(output=manifest_error, is_error=True)

    # Commit staged writes
    writes = manifest.get("writes", [])
    committed = 0
    errors = []
    warnings = []
    if team_contract is not None and team_contract.get("verification_surface_write_enforcement") == "warn":
        write_paths = [
            rel
            for rel in (
                _normalize_repo_relative_path(
                    item.get("path"),
                    str(team_contract.get("repo_root") or ""),
                )
                for item in writes
                if isinstance(item, dict)
            )
            if rel
        ]
        verify_writes = _verification_surface_warning_paths(
            write_paths,
            allowed_write_paths=set(team_contract.get("owned_files") or ())
            | set(team_contract.get("touches_paths") or ()),
            verify_paths=set(team_contract.get("verify_paths") or ()),
        )
        if verify_writes:
            warnings.append(
                "daytona_codeact: verification-surface writes allowed in advisory mode. "
                f"Observed write(s) on verification paths: {', '.join(verify_writes[:3])}."
            )

    for w in writes:
        path = w.get("path", "")
        content = w.get("content", "")
        try:
            await sandbox.fs.upload_file(content.encode("utf-8"), path)
            prime_cache_after_write(context, path, content)
            record_edit_in_ledger(context, path, edit_type="codeact")
            committed += 1
        except Exception as exc:
            errors.append(f"{path}: {exc}")

    # Build output
    shells = manifest.get("shells", [])
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
            "warnings": warnings,
            "error": manifest.get("error", "")[:500] if manifest.get("error") else "",
        }
    )

    return ToolResult(
        output=output,
        is_error=bool(errors),
        metadata={
            "status": manifest.get("status", "unknown"),
            "files_written": committed,
            "shells_run": len(shells),
        },
    )

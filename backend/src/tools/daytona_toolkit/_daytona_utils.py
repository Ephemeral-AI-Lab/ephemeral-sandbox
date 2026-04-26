"""Shared utilities for sandbox tools."""

from __future__ import annotations

import asyncio
import base64
import inspect
import json
import logging
import re
import shlex
import uuid
from typing import Any

from tools.core.base import ToolExecutionContextService

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_DEFAULT_TIMEOUT = 120
_OUTPUT_MAX_CHARS = 8000
_EXIT_MARKER = "__CODEX_EXIT_CODE__="
_SANDBOX_RECOVERY_KEY = "daytona_recovery_attempts"
_SANDBOX_RECOVERY_PATTERNS = (
    "no such container",
    "container not found",
    "sandbox container not found",
)
_VERIFY_PATH_RE = re.compile(r"(?<![A-Za-z0-9_./-])([A-Za-z0-9_./-]+\.py)(?![A-Za-z0-9_./-])")
_USER_LOCAL_BIN_EXPORT = 'export PATH="$HOME/.local/bin:$PATH"'
_PROJECT_VENV_BIN_EXPORT = 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi'
_PYTHON3_SHIM = (
    'if ! command -v python >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; '
    'then python() { command python3 "$@"; }; fi'
)
_TRAILING_TERM_NOISE_RE = re.compile(r"(?:\x1b\[[0-9;]*[A-Za-z]|TERM environment variable not set\.)+\s*$")
_TEST_PATH_COMPONENTS = {"test", "tests", "__tests__"}
_TEST_FILE_ALLOW_METADATA_KEYS = ("allow_test_file_edits", "allow_test_file_writes")
_TEST_FILE_SUFFIXES = (
    "_test.py",
    "_spec.py",
    "-test.py",
    "-spec.py",
)
_REMOTE_WRITE_CHUNK_BYTES = 24 * 1024


# ---------------------------------------------------------------------------
# Output formatting
# ---------------------------------------------------------------------------


def _truncate(text: str, max_chars: int = _OUTPUT_MAX_CHARS) -> str:
    if len(text) <= max_chars:
        return text
    half = max_chars // 2
    return text[:half] + f"\n\n... truncated ({len(text)} chars total) ...\n\n" + text[-half:]


def _truncate_tail(text: str, max_chars: int = _OUTPUT_MAX_CHARS) -> str:
    if len(text) <= max_chars:
        return text
    return f"... truncated ({len(text)} chars total, showing last {max_chars}) ...\n\n{text[-max_chars:]}"


def _format_shell_stdout(text: str, *, exit_code: int, max_chars: int = _OUTPUT_MAX_CHARS) -> str:
    """Trim shell output for the model."""
    if exit_code != 0:
        return _truncate_tail(text, max_chars=max_chars)
    return _truncate(text, max_chars=max_chars)


# ---------------------------------------------------------------------------
# Bash command wrapping
# ---------------------------------------------------------------------------


def _wrap_bash_command(command: str, *, cwd: str | None = None) -> str:
    """Wrap *command* so we can recover exit code even if the SDK omits it."""
    cd_command = f"cd {shlex.quote(cwd)}\n" if cwd else ""
    script = (
        f"{_USER_LOCAL_BIN_EXPORT}\n"
        f"{cd_command}"
        f"{_PROJECT_VENV_BIN_EXPORT}\n"
        f"{_PYTHON3_SHIM}\n"
        f"{command}\n"
        "__codex_exit_code=$?\n"
        f'printf "\\n{_EXIT_MARKER}%s\\n" "$__codex_exit_code"\n'
        'exit "$__codex_exit_code"'
    )
    return f"env -u LC_ALL bash -o pipefail -lc {shlex.quote(script)}"


def _extract_exit_code(
    output: str,
    *,
    fallback_exit_code: int | None,
) -> tuple[str, int]:
    """Strip the synthetic exit marker and return the resolved exit code."""
    sanitized = _TRAILING_TERM_NOISE_RE.sub("", output or "").rstrip()
    matches = list(re.finditer(rf"\n?{re.escape(_EXIT_MARKER)}(-?\d+)", sanitized, flags=re.S))
    if matches:
        marker = matches[-1]
        resolved = int(marker.group(1))
        cleaned = sanitized[: marker.start()]
        if cleaned.endswith("\n"):
            cleaned = cleaned[:-1]
        return cleaned, resolved
    if fallback_exit_code is None:
        return sanitized, 0
    if isinstance(fallback_exit_code, int):
        return sanitized, fallback_exit_code
    if isinstance(fallback_exit_code, str):
        stripped = fallback_exit_code.strip()
        if stripped.lstrip("-").isdigit():
            return sanitized, int(stripped)
    return sanitized, 0


# ---------------------------------------------------------------------------
# Sandbox context helpers
# ---------------------------------------------------------------------------


def _sandbox_context_error(detail: str | None = None) -> str:
    base = (
        "No sandbox in context. "
        "Ensure tool context was initialized with a valid sandbox_id."
    )
    if detail:
        return f"{base} Last recovery error: {detail}"
    return base


def _is_recoverable_sandbox_error(exc: Exception) -> bool:
    text = str(exc).lower()
    return any(pattern in text for pattern in _SANDBOX_RECOVERY_PATTERNS)


async def _attach_sandbox_to_context(context: ToolExecutionContextService) -> Any:
    """Lazily attach sandbox + CI when prepare_context did not complete."""
    sandbox_id = str(context.get("sandbox_id") or "").strip()
    if not sandbox_id:
        raise RuntimeError(_sandbox_context_error())
    try:
        from sandbox.async_client import get_async_sandbox
        from sandbox.workspace import (
            discover_workspace_async,
            ensure_code_intelligence_runtime,
        )

        sandbox = await get_async_sandbox(sandbox_id)
        repo_root = context.get("repo_root")
        if not repo_root:
            project_dir = getattr(sandbox, "project_dir", None)
            repo_root = project_dir or await discover_workspace_async(sandbox)
        ensure_code_intelligence_runtime(
            context,
            sandbox_id=sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )
        return sandbox
    except Exception as exc:
        raise RuntimeError(_sandbox_context_error(str(exc))) from exc


async def _require_sandbox(context: ToolExecutionContextService) -> Any:
    sandbox = context.get("daytona_sandbox")
    if sandbox is not None:
        return sandbox
    return await _attach_sandbox_to_context(context)


async def _recover_sandbox(context: ToolExecutionContextService, exc: Exception) -> Any:
    """Restart/rebind the sandbox once after container-loss style failures."""
    if not _is_recoverable_sandbox_error(exc):
        raise exc
    attempts_value = context.get(_SANDBOX_RECOVERY_KEY, 0)
    try:
        attempts = int(attempts_value)
    except (TypeError, ValueError):
        attempts = 0
    if attempts >= 1:
        raise exc
    sandbox_id = str(context.get("sandbox_id") or "").strip()
    if not sandbox_id:
        raise exc
    context[_SANDBOX_RECOVERY_KEY] = attempts + 1
    logger.warning(
        "Recovering sandbox %s after tool failure: %s",
        sandbox_id,
        exc,
    )
    try:
        from sandbox.service import SandboxService

        await asyncio.to_thread(SandboxService().ensure_sandbox_running, sandbox_id)
    finally:
        context["daytona_sandbox"] = None
        context["ci_service"] = None
    recovered = await _attach_sandbox_to_context(context)
    logger.warning("Recovered sandbox %s and retrying tool once", sandbox_id)
    return recovered


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------


def _path_error(exc: Exception, path: str) -> str | None:
    """Return a human-readable message if *exc* is a path-not-found error, else None."""
    msg = str(exc)
    if isinstance(exc, FileNotFoundError) or "No such file or directory" in msg:
        return f"Path does not exist: {path}"
    # The sandbox SDK wraps errors and may lose the inner message.
    _sdk_prefixes = ("Failed to list files", "Failed to upload files", "Failed to download")
    if any(msg.startswith(p) for p in _sdk_prefixes) and msg.rstrip().endswith(":"):
        return f"Path does not exist: {path}"
    return None


def _get_repo_root(context: ToolExecutionContextService) -> str | None:
    """Return the canonical sandbox repo root for file-oriented tools."""
    return context.get("repo_root")


def _get_exec_cwd(context: ToolExecutionContextService) -> str | None:
    """Return the working directory for shell execution."""
    return (
        context.get("exec_cwd")
        or _get_repo_root(context)
    )


def _resolve_path(path: str, context: ToolExecutionContextService) -> str:
    """Resolve a relative path against the sandbox repo root."""
    if path.startswith("/"):
        return path
    repo_root = _get_repo_root(context)
    if repo_root:
        return f"{repo_root}/{path}"
    return path


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


def _verification_surface_enforcement_mode(context: ToolExecutionContextService) -> str:
    raw = (
        str(
            context.get("verification_surface_write_enforcement")
            or context.get("verification_surface_policy")
            or ""
        )
        .strip()
        .lower()
    )
    if raw in {"warn", "warning", "soft", "advisory"}:
        return "warn"
    return "error"


def _metadata_flag_enabled(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "on"}
    return bool(value)


def _test_file_edits_allowed(context: ToolExecutionContextService) -> bool:
    return any(
        _metadata_flag_enabled(context.get(key))
        for key in _TEST_FILE_ALLOW_METADATA_KEYS
    )


def _is_test_file_path(rel_path: str) -> bool:
    parts = [part for part in rel_path.replace("\\", "/").split("/") if part]
    if not parts:
        return False
    lowered_parts = {part.lower() for part in parts[:-1]}
    if lowered_parts & _TEST_PATH_COMPONENTS:
        return True
    basename = parts[-1].lower()
    # Repo-root conftest.py is the project's pytest configuration owner,
    # not a test file. Only treat conftest.py as a test file when it sits
    # inside a tests/ directory (already caught by the parts check above).
    if basename == "conftest.py":
        return False
    return (
        basename.startswith("test_")
        or basename.startswith("test-")
        or basename.endswith(_TEST_FILE_SUFFIXES)
        or ".test." in basename
        or ".spec." in basename
    )


def _supports_exec_transport(sandbox: Any) -> bool:
    process = getattr(sandbox, "process", None)
    exec_fn = getattr(process, "exec", None) if process is not None else None
    return callable(exec_fn)


async def _exec_command(sandbox: Any, command: str, *, timeout: int | None = None) -> Any:
    process = getattr(sandbox, "process", None)
    exec_fn = getattr(process, "exec", None) if process is not None else None
    if not callable(exec_fn):
        raise RuntimeError("Sandbox process has no exec method")
    if not inspect.iscoroutinefunction(exec_fn):
        raise RuntimeError("Sandbox process.exec must be async")
    return await exec_fn(command, timeout=timeout) if timeout is not None else await exec_fn(command)


def _build_read_text_file_command(file_path: str) -> str:
    script = """
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    content = path.read_text(encoding="utf-8")
except FileNotFoundError:
    print(json.dumps({"exists": False}))
else:
    print(json.dumps({"exists": True, "content": content}))
"""
    return (
        f"python3 -c {shlex.quote(script)} {shlex.quote(file_path)}"
    )


def _build_write_text_file_command(file_path: str, content: str) -> str:
    payload = base64.b64encode(content.encode("utf-8")).decode("ascii")
    script = """
import base64
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(base64.b64decode(sys.argv[2]).decode("utf-8"), encoding="utf-8")
"""
    return (
        f"python3 -c {shlex.quote(script)} "
        f"{shlex.quote(file_path)} {shlex.quote(payload)}"
    )


def _build_truncate_text_file_command(file_path: str) -> str:
    script = """
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_bytes(b"")
"""
    return f"python3 -c {shlex.quote(script)} {shlex.quote(file_path)}"


def _build_append_text_file_chunk_command(file_path: str, payload: str) -> str:
    script = """
import base64
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
with path.open("ab") as handle:
    handle.write(base64.b64decode(sys.argv[2]))
"""
    return (
        f"python3 -c {shlex.quote(script)} "
        f"{shlex.quote(file_path)} {shlex.quote(payload)}"
    )


def _build_replace_file_command(tmp_path: str, file_path: str) -> str:
    script = """
import os
import pathlib
import sys

tmp = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
os.replace(tmp, dst)
"""
    return (
        f"python3 -c {shlex.quote(script)} "
        f"{shlex.quote(tmp_path)} {shlex.quote(file_path)}"
    )


def _build_remove_file_command(file_path: str) -> str:
    return f"rm -f {shlex.quote(file_path)}"


def _build_write_text_file_commands(
    file_path: str,
    content: str,
    *,
    chunk_bytes: int = _REMOTE_WRITE_CHUNK_BYTES,
) -> tuple[list[str], str | None]:
    """Build remote write commands for small or large files."""
    data = content.encode("utf-8")
    if len(data) <= chunk_bytes:
        return [_build_write_text_file_command(file_path, content)], None

    tmp_path = f"{file_path}.codex-write-{uuid.uuid4().hex}.tmp"
    commands = [_build_truncate_text_file_command(tmp_path)]
    for index in range(0, len(data), chunk_bytes):
        chunk = data[index : index + chunk_bytes]
        payload = base64.b64encode(chunk).decode("ascii")
        commands.append(_build_append_text_file_chunk_command(tmp_path, payload))
    commands.append(_build_replace_file_command(tmp_path, file_path))
    return commands, tmp_path


async def _read_text_file_via_exec(
    sandbox: Any,
    file_path: str,
    *,
    allow_missing: bool = False,
) -> tuple[str, bool]:
    response = await _exec_command(
        sandbox,
        _wrap_bash_command(_build_read_text_file_command(file_path)),
    )
    stdout = getattr(response, "result", "") or ""
    cleaned, exit_code = _extract_exit_code(
        stdout,
        fallback_exit_code=getattr(response, "exit_code", None),
    )
    if exit_code not in (0, None):
        raise RuntimeError(cleaned or f"read failed for {file_path}")
    payload = json.loads(cleaned or "{}")
    if not payload.get("exists"):
        if allow_missing:
            return "", False
        raise FileNotFoundError(file_path)
    return str(payload.get("content", "") or ""), True


async def _write_text_file_via_exec(
    sandbox: Any,
    file_path: str,
    content: str,
    *,
    timeout: int | None = None,
) -> None:
    if not _supports_exec_transport(sandbox):
        raise RuntimeError("Sandbox process has no exec method")
    commands, tmp_path = _build_write_text_file_commands(file_path, content)
    try:
        for command in commands:
            response = await _exec_command(
                sandbox,
                _wrap_bash_command(command),
                timeout=timeout,
            )
            stdout = getattr(response, "result", "") or ""
            cleaned, exit_code = _extract_exit_code(
                stdout,
                fallback_exit_code=getattr(response, "exit_code", None),
            )
            if exit_code not in (0, None):
                raise RuntimeError(cleaned or f"write failed for {file_path}")
    except Exception:
        if tmp_path:
            try:
                await _exec_command(
                    sandbox,
                    _wrap_bash_command(_build_remove_file_command(tmp_path)),
                    timeout=timeout,
                )
            except Exception:
                logger.debug("remote temp cleanup failed for %s", tmp_path, exc_info=True)
        raise


async def _delete_file_via_exec(sandbox: Any, file_path: str) -> None:
    if not _supports_exec_transport(sandbox):
        raise RuntimeError("Sandbox process has no exec method")
    response = await _exec_command(sandbox, _wrap_bash_command(f"rm -f {shlex.quote(file_path)}"))
    stdout = getattr(response, "result", "") or ""
    cleaned, exit_code = _extract_exit_code(
        stdout,
        fallback_exit_code=getattr(response, "exit_code", None),
    )
    if exit_code not in (0, None):
        raise RuntimeError(cleaned or f"delete failed for {file_path}")

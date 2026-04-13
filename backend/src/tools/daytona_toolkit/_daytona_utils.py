"""Shared utilities for Daytona sandbox tools — extracted from tools.py to reduce duplication."""

from __future__ import annotations

import asyncio
import logging
import re
import shlex
import time
from typing import TYPE_CHECKING, Any

from config.defaults import DEFAULT_SANDBOX_CI_ROOT, DEFAULT_TEAM_SAFE_AGENT_NAMES
from tools.core.base import ToolExecutionContext, ToolResult

if TYPE_CHECKING:
    from tools.core.decorator import tool

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
_PYTHON3_SHIM = 'if command -v python3 >/dev/null 2>&1; then python() { command python3 "$@"; }; fi'


# ---------------------------------------------------------------------------
# Coordination helpers
# ---------------------------------------------------------------------------


def is_coordinated_team_agent(context: ToolExecutionContext) -> bool:
    """True when the current agent is in the team-safe set AND team mode is active."""
    agent_name = str(context.metadata.get("agent_name") or "").strip()
    if agent_name not in DEFAULT_TEAM_SAFE_AGENT_NAMES:
        return False
    return bool(context.metadata.get("team_mode_enabled"))


def record_coordination_warning(
    context: ToolExecutionContext,
    *,
    category: str,
    message: str,
) -> None:
    """Persist a coordination warning on the live tool context.

    Warnings are advisory, but they taint the current task packet so posthook
    tools can steer the agent toward ``request_replan()`` instead of reporting
    success after a scope mismatch.
    """
    raw = context.metadata.get("coordination_warnings")
    warnings: list[dict[str, Any]]
    if isinstance(raw, list):
        warnings = raw
    else:
        warnings = []
        context.metadata["coordination_warnings"] = warnings

    normalized_category = str(category or "").strip() or "coordination"
    normalized_message = str(message or "").strip()
    if not normalized_message:
        return
    for item in warnings:
        if not isinstance(item, dict):
            continue
        if (
            str(item.get("category") or "").strip() == normalized_category
            and str(item.get("message") or "").strip() == normalized_message
        ):
            return
    warnings.append(
        {
            "category": normalized_category,
            "message": normalized_message,
            "timestamp": time.time(),
        }
    )
    context.metadata["coordination_warning_present"] = True


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
    """Format shell stdout for model consumption.

    Successful commands keep head+tail context, but failing commands keep the
    tail because test runners typically print the actionable failure details at
    the end of stdout.
    """
    if exit_code != 0:
        return _truncate_tail(text, max_chars=max_chars)
    return _truncate(text, max_chars=max_chars)


# ---------------------------------------------------------------------------
# Bash command wrapping
# ---------------------------------------------------------------------------


def _wrap_bash_command(command: str) -> str:
    """Wrap *command* so we can recover exit code even if the SDK omits it."""
    script = (
        f"{_USER_LOCAL_BIN_EXPORT}\n"
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
    match = re.search(rf"\n?{re.escape(_EXIT_MARKER)}(-?\d+)\s*$", output, flags=re.S)
    if match:
        resolved = int(match.group(1))
        cleaned = output[: match.start()]
        if cleaned.endswith("\n"):
            cleaned = cleaned[:-1]
        return cleaned, resolved
    return output, 0 if fallback_exit_code is None else int(fallback_exit_code)


# ---------------------------------------------------------------------------
# Sandbox context helpers
# ---------------------------------------------------------------------------


def _sandbox_context_error(detail: str | None = None) -> str:
    base = (
        "No Daytona sandbox in context. "
        "Ensure DaytonaToolkit was initialized with a valid sandbox_id."
    )
    if detail:
        return f"{base} Last recovery error: {detail}"
    return base


def _is_recoverable_sandbox_error(exc: Exception) -> bool:
    text = str(exc).lower()
    return any(pattern in text for pattern in _SANDBOX_RECOVERY_PATTERNS)


async def _attach_sandbox_to_context(context: ToolExecutionContext) -> Any:
    """Lazily attach sandbox + CI when prepare_context did not complete."""
    sandbox_id = str(context.metadata.get("sandbox_id") or "").strip()
    if not sandbox_id:
        raise RuntimeError(_sandbox_context_error())
    try:
        from sandbox.async_client import get_async_sandbox
        from sandbox.workspace import discover_workspace_async, inject_code_intelligence

        sandbox = await get_async_sandbox(sandbox_id)
        context.metadata["daytona_sandbox"] = sandbox
        cwd = context.metadata.get("daytona_cwd")
        if not cwd:
            project_dir = getattr(sandbox, "project_dir", None)
            cwd = project_dir or await discover_workspace_async(sandbox)
            if cwd:
                context.metadata["daytona_cwd"] = cwd
        if "ci_service" not in context.metadata and not context.metadata.get(
            "skip_code_intelligence"
        ):
            ci_root = context.metadata.get("ci_workspace_root") or cwd or DEFAULT_SANDBOX_CI_ROOT
            inject_code_intelligence(context, sandbox_id, sandbox, ci_root)
        return sandbox
    except Exception as exc:
        raise RuntimeError(_sandbox_context_error(str(exc))) from exc


async def _require_sandbox(context: ToolExecutionContext) -> Any:
    sandbox = context.metadata.get("daytona_sandbox")
    if sandbox is not None:
        return sandbox
    return await _attach_sandbox_to_context(context)


async def _recover_sandbox(context: ToolExecutionContext, exc: Exception) -> Any:
    """Restart/rebind the sandbox once after container-loss style failures."""
    if not _is_recoverable_sandbox_error(exc):
        raise exc
    attempts = int(context.metadata.get(_SANDBOX_RECOVERY_KEY) or 0)
    if attempts >= 1:
        raise exc
    sandbox_id = str(context.metadata.get("sandbox_id") or "").strip()
    if not sandbox_id:
        raise exc
    context.metadata[_SANDBOX_RECOVERY_KEY] = attempts + 1
    logger.warning(
        "Recovering Daytona sandbox %s after tool failure: %s",
        sandbox_id,
        exc,
    )
    try:
        from sandbox.service import SandboxService

        await asyncio.to_thread(SandboxService().ensure_sandbox_running, sandbox_id)
    finally:
        context.metadata["daytona_sandbox"] = None
        context.metadata["ci_service"] = None
    recovered = await _attach_sandbox_to_context(context)
    logger.warning("Recovered Daytona sandbox %s and retrying tool once", sandbox_id)
    return recovered


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------


def _path_error(exc: Exception, path: str) -> str | None:
    """Return a human-readable message if *exc* is a path-not-found error, else None."""
    msg = str(exc)
    if isinstance(exc, FileNotFoundError) or "No such file or directory" in msg:
        return f"Path does not exist: {path}"
    # Daytona SDK wraps errors and may lose the inner message
    _sdk_prefixes = ("Failed to list files", "Failed to upload files", "Failed to download")
    if any(msg.startswith(p) for p in _sdk_prefixes) and msg.rstrip().endswith(":"):
        return f"Path does not exist: {path}"
    return None


def _get_cwd(context: ToolExecutionContext) -> str | None:
    """Get working directory, preferring sandbox project dir.

    Returns None if no sandbox-specific cwd is set, letting the sandbox
    use its default directory (typically /home/daytona).
    """
    return context.metadata.get("daytona_cwd")


def _resolve_path(path: str, context: ToolExecutionContext) -> str:
    """Resolve a relative path against the sandbox cwd.

    Absolute paths are returned as-is. Relative paths are joined
    with the sandbox cwd (detected via pwd on first connect).
    """
    if path.startswith("/"):
        return path
    cwd = _get_cwd(context)
    if cwd:
        return f"{cwd}/{path}"
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


def _verification_surface_paths(
    context: ToolExecutionContext,
    repo_root: str,
) -> list[str]:
    """Return repo-relative verification-surface paths named in the task metadata."""
    raw_candidates = (
        context.metadata.get("owned_failures"),
        context.metadata.get("benchmark_test_files"),
        context.metadata.get("benchmark_test_ids"),
        context.metadata.get("verify"),
        context.metadata.get("verification"),
        context.metadata.get("retries"),
        context.metadata.get("reproduction"),
    )
    collected: list[str] = []
    for value in raw_candidates:
        collected.extend(_normalize_string_list(value, repo_root))
        collected.extend(_extract_verify_paths(value, repo_root))
    deduped: list[str] = []
    seen: set[str] = set()
    for item in collected:
        if not item or item in seen:
            continue
        seen.add(item)
        deduped.append(item)
    return deduped


def _is_verification_surface_path(
    context: ToolExecutionContext,
    rel_path: str,
    repo_root: str,
) -> bool:
    """True when *rel_path* belongs to an explicitly named verification surface."""
    verification_paths = _verification_surface_paths(context, repo_root)
    if not verification_paths:
        return False
    return _path_under_write_scope(rel_path, verification_paths)


# ---------------------------------------------------------------------------
# Write-scope helpers
# ---------------------------------------------------------------------------


def _verification_surface_enforcement_mode(context: ToolExecutionContext) -> str:
    raw = (
        str(
            context.metadata.get("verification_surface_write_enforcement")
            or context.metadata.get("verification_surface_policy")
            or ""
        )
        .strip()
        .lower()
    )
    if raw in {"warn", "warning", "soft", "advisory"}:
        return "warn"
    return "error"


def _normalize_write_scope(raw: Any, repo_root: str) -> list[str]:
    """Normalize a ``write_scope`` list to repo-relative prefixes."""
    if not isinstance(raw, list):
        return []
    out: list[str] = []
    for item in raw:
        if not isinstance(item, str):
            continue
        normed = _normalize_repo_relative_path(item.rstrip("/"), repo_root)
        if normed:
            out.append(normed)
    return out


def _path_under_write_scope(rel_path: str, write_scope: list[str]) -> bool:
    """Return True if *rel_path* falls under any prefix in *write_scope*."""
    for prefix in write_scope:
        if rel_path == prefix or rel_path.startswith(prefix.rstrip("/") + "/"):
            return True
    return False


def _team_repo_write_error(
    context: ToolExecutionContext,
    file_path: str,
    *,
    tool_name: str,
) -> str | None:
    """Block writes for validator lanes only; write_scope is advisory."""
    if not is_coordinated_team_agent(context):
        return None
    repo_root = str(_get_cwd(context) or "")
    rel_path = _normalize_repo_relative_path(file_path, repo_root)
    if not rel_path:
        return None
    if str(context.metadata.get("agent_name") or "").strip() == "validator":
        return (
            f"{tool_name}: validator lanes must not write repository files "
            f"({rel_path})."
        )
    return None


def _team_repo_write_warning(
    context: ToolExecutionContext,
    file_path: str,
    *,
    tool_name: str,
) -> str | None:
    """Advisory warning for writes outside write_scope."""
    if not is_coordinated_team_agent(context):
        return None
    repo_root = str(_get_cwd(context) or "")
    rel_path = _normalize_repo_relative_path(file_path, repo_root)
    if not rel_path:
        return None
    write_scope = _normalize_write_scope(context.metadata.get("write_scope"), repo_root)
    if not write_scope:
        return None  # no write_scope set — unconstrained
    if _path_under_write_scope(rel_path, write_scope):
        return None
    return f"{tool_name}: write to {rel_path} is outside write_scope {write_scope} (advisory)."


# ---------------------------------------------------------------------------
# Upload helper
# ---------------------------------------------------------------------------


async def _upload_file_compat(sandbox: Any, content: bytes, file_path: str) -> None:
    """Upload using the SDK signature, with fallback for stale path-first mocks."""
    try:
        await sandbox.fs.upload_file(content, file_path)
    except (AttributeError, TypeError) as exc:
        if "decode" not in str(exc) and "bytes-like object" not in str(exc):
            raise
        await sandbox.fs.upload_file(file_path, content)

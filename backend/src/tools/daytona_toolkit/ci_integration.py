"""Daytona-specific CI integration helpers."""

from __future__ import annotations

import logging
import os
import re
import shlex
from typing import Any

from team._path_utils import normalize_scope_paths
from tools.core.ci_runtime import sync_deleted_file, sync_write_to_ci
from tools.core.sandbox_runtime import (
    get_daytona_cwd,
    get_daytona_sandbox,
    require_declared_shell_outputs,
)
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import (
    _team_repo_write_error,
    _team_repo_write_warning,
    is_coordinated_team_agent,
    record_coordination_warning,
)

logger = logging.getLogger(__name__)

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
# Hard-blocked destructive shell commands — matches the pattern in codeact_tool.py
# _WRAPPER_TEMPLATE. Exported so callers can pre-screen commands before sandbox
# execution.
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


def destructive_shell_command_error(command: str) -> str | None:
    """Return an error if the command is an unconditionally blocked destructive operation.

    This is checked *before* coordination-mode gates — destructive commands
    are always blocked regardless of team mode or declared_output_paths.
    """
    if _DESTRUCTIVE_SHELL_PATTERN.search(command or ""):
        return (
            "BLOCKED: destructive shell command that targets workspace or system "
            "directories (rm -r /testbed, mv /testbed, etc.) is forbidden. "
            "These commands destroy the shared workspace and cannot be undone. "
            "Use targeted file operations instead."
        )
    return None


def shell_mutation_declaration_error(
    context: ToolExecutionContext,
    *,
    command: str,
    declared_output_paths: list[str] | None,
) -> str | None:
    """Return an error when a mutating shell command lacks declared outputs."""
    # Unconditional hard block for destructive commands — not overridable.
    destructive_err = destructive_shell_command_error(command)
    if destructive_err is not None:
        return destructive_err
    if not require_declared_shell_outputs(context) and not is_coordinated_team_agent(context):
        return None
    if not command_may_mutate_workspace(command):
        return None
    if normalize_scope_paths(declared_output_paths or []):
        return None
    return (
        "Mutating shell calls must declare `declared_output_paths` in team "
        "coordination mode. Prefer daytona_write_file/daytona_edit_file, or list every "
        "path the command may create, modify, move, or delete before running it."
    )


def command_may_mutate_workspace(command: str) -> bool:
    """Heuristic gate for when a shell command should trigger CI reconciliation."""
    stripped = (command or "").strip()
    if not stripped:
        return False
    # Treat test execution as read-only for coordination purposes even if the
    # tool runner writes ephemeral caches like .pytest_cache internally.
    if _READ_ONLY_TEST_COMMAND_PATTERN.match(stripped):
        return False
    return bool(_SHELL_MUTATION_PATTERN.search(stripped))


async def sync_shell_mutations(
    context: ToolExecutionContext,
    *,
    command: str,
    declared_output_paths: list[str] | None = None,
    limit: int = 64,
) -> dict[str, Any]:
    """Refresh CI state for files currently dirty after a mutating shell command.

    This is intentionally conservative: it only runs for commands that look
    mutating and only when the sandbox cwd is a git checkout. The goal is to
    keep CI caches and hotspots in sync when an
    agent edits files via shell commands instead of structured edit tools.
    """
    declared_output_paths = normalize_scope_paths(declared_output_paths or [])
    missing_decl = shell_mutation_declaration_error(
        context,
        command=command,
        declared_output_paths=declared_output_paths,
    )
    if missing_decl is not None:
        return {
            "enabled": False,
            "files": 0,
            "truncated": False,
            "missing_declarations": True,
            "error": missing_decl,
        }
    if not command_may_mutate_workspace(command) and not declared_output_paths:
        return {"enabled": False, "files": 0, "truncated": False}

    sandbox = get_daytona_sandbox(context)
    cwd = get_daytona_cwd(context)
    if sandbox is None or not cwd:
        return {"enabled": False, "files": 0, "truncated": False}

    try:
        root_resp = await sandbox.process.exec(
            f"git -C {shlex.quote(cwd)} rev-parse --show-toplevel",
            timeout=20,
        )
    except Exception:
        logger.debug("Shell sync skipped: could not resolve git root for %s", cwd, exc_info=True)
        return {"enabled": False, "files": 0, "truncated": False}

    git_root = (getattr(root_resp, "result", "") or "").strip()
    if getattr(root_resp, "exit_code", 1) != 0 or not git_root:
        return {"enabled": False, "files": 0, "truncated": False}

    if declared_output_paths:
        dirty_paths = [
            path if path.startswith("/") else os.path.normpath(f"{git_root}/{path}")
            for path in declared_output_paths
        ]
    else:
        try:
            status_resp = await sandbox.process.exec(
                f"git -C {shlex.quote(git_root)} status --porcelain --untracked-files=all",
                timeout=30,
            )
        except Exception:
            logger.debug("Shell sync skipped: git status failed for %s", git_root, exc_info=True)
            return {"enabled": True, "files": 0, "truncated": False}

        if getattr(status_resp, "exit_code", 1) != 0:
            return {"enabled": True, "files": 0, "truncated": False}

        dirty_paths = _parse_git_status_paths((getattr(status_resp, "result", "") or ""), git_root)
    truncated = len(dirty_paths) > limit
    changed_count = 0
    write_errors: list[str] = []
    write_warnings: list[str] = []
    for file_path in dirty_paths[:limit]:
        contract_error = _team_repo_write_error(
            context,
            file_path,
            tool_name="shell_mutation",
        )
        if contract_error is not None:
            write_errors.append(contract_error)
            continue
        contract_warning = _team_repo_write_warning(
            context,
            file_path,
            tool_name="shell_mutation",
        )
        if contract_warning is not None:
            write_warnings.append(contract_warning)
            record_coordination_warning(
                context,
                category="write_scope",
                message=contract_warning,
            )
        try:
            raw = await sandbox.fs.download_file(file_path)
        except Exception:
            sync_deleted_file(
                context,
                file_path,
                edit_type="shell_mutation",
                description=f"Shell command: {command[:160]}",
            )
            changed_count += 1
            continue

        content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
        sync_write_to_ci(
            context,
            file_path,
            content,
            edit_type="shell_mutation",
            description=f"Shell command: {command[:160]}",
        )
        changed_count += 1

    return {
        "enabled": True,
        "files": changed_count,
        "truncated": truncated,
        "declared_output_paths": declared_output_paths,
        "write_errors": write_errors,
        "write_warnings": write_warnings,
    }


def _parse_git_status_paths(output: str, git_root: str) -> list[str]:
    """Parse ``git status --porcelain`` output into absolute paths."""
    seen: set[str] = set()
    paths: list[str] = []
    for raw_line in output.splitlines():
        line = raw_line.rstrip()
        if len(line) < 4:
            continue
        status = line[:2]
        payload = line[3:].strip()
        if not payload:
            continue
        candidates = payload.split(" -> ") if " -> " in payload else [payload]
        for rel_path in candidates:
            abs_path = os.path.normpath(os.path.join(git_root, rel_path))
            if abs_path in seen:
                continue
            seen.add(abs_path)
            paths.append(abs_path)
            if "D" in status:
                break
    return paths


# ---------------------------------------------------------------------------
# Layer 2: Post-shell workspace regression detection
# ---------------------------------------------------------------------------


async def snapshot_dirty_files(
    context: ToolExecutionContext,
) -> set[str] | None:
    """Capture the set of dirty file paths in the sandbox working tree.

    Returns ``None`` if the sandbox is unavailable or not a git checkout
    (callers should treat ``None`` as "skip regression check").
    """
    sandbox = get_daytona_sandbox(context)
    cwd = get_daytona_cwd(context)
    if sandbox is None or not cwd:
        return None
    try:
        root_resp = await sandbox.process.exec(
            f"git -C {shlex.quote(cwd)} rev-parse --show-toplevel",
            timeout=20,
        )
    except Exception:
        return None
    git_root = (getattr(root_resp, "result", "") or "").strip()
    if getattr(root_resp, "exit_code", 1) != 0 or not git_root:
        return None
    try:
        status_resp = await sandbox.process.exec(
            f"git -C {shlex.quote(git_root)} status --porcelain --untracked-files=all",
            timeout=30,
        )
    except Exception:
        return None
    if getattr(status_resp, "exit_code", 1) != 0:
        return None
    paths = _parse_git_status_paths(
        (getattr(status_resp, "result", "") or ""), git_root,
    )
    return set(paths)


async def detect_workspace_regression(
    context: ToolExecutionContext,
    *,
    pre_snapshot: set[str] | None,
) -> list[str]:
    """Compare pre- and post-execution dirty files to find regressions.

    A *regression* is a file that was dirty before the shell command(s) ran
    but is clean afterward — meaning the command silently reverted tracked
    work (e.g. ``git stash``, ``git checkout -- .``).

    Returns a list of absolute paths that regressed. Empty list means no
    regression detected (or snapshot was unavailable).
    """
    if pre_snapshot is None or not pre_snapshot:
        return []
    post_snapshot = await snapshot_dirty_files(context)
    if post_snapshot is None:
        # Can't verify — sandbox or git unavailable after execution.
        return []
    regressed = sorted(pre_snapshot - post_snapshot)
    if regressed:
        logger.warning(
            "Workspace regression detected: %d file(s) reverted by shell command: %s",
            len(regressed),
            ", ".join(regressed[:10]),
        )
    return regressed


__all__ = [
    "command_may_mutate_workspace",
    "destructive_shell_command_error",
    "detect_workspace_regression",
    "shell_mutation_declaration_error",
    "snapshot_dirty_files",
    "sync_shell_mutations",
]

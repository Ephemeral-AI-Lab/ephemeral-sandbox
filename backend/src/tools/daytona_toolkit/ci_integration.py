"""CI integration helpers for the Daytona toolkit.

Provides service acquisition, tree cache priming after writes,
lightweight shell-mutation reconciliation, and atlas dirty-marking.
All CI features are optional — tools degrade gracefully if no CI service
is configured.
"""

from __future__ import annotations

import logging
import os
import re
import shlex
from typing import Any

from tools.daytona_toolkit.coordination import (
    build_scope_packet_for_context,
    normalize_scope_paths,
)
from tools.core.base import ToolExecutionContext

logger = logging.getLogger(__name__)
_DEFAULT_SCOPE_RECENT_SECONDS = 300.0

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


def get_ci_service(context: ToolExecutionContext) -> Any | None:
    """Get the CodeIntelligenceService from context, or None if unavailable."""
    return context.metadata.get("ci_service")


def get_daytona_sandbox(context: ToolExecutionContext) -> Any | None:
    """Get the injected Daytona sandbox object, if available."""
    return context.metadata.get("daytona_sandbox")


def get_daytona_cwd(context: ToolExecutionContext) -> str:
    """Get the injected Daytona working directory, if available."""
    return context.metadata.get("daytona_cwd") or ""


def require_declared_shell_outputs(context: ToolExecutionContext) -> bool:
    """Return True when mutating shell commands must predeclare output paths."""
    return bool(context.metadata.get("require_declared_shell_outputs"))


def scope_paths_for_write(
    context: ToolExecutionContext,
    *,
    fallback_paths: list[str] | None = None,
) -> list[str]:
    """Return the scope paths a write should be validated against."""
    baseline = context.metadata.get("scope_packet")
    if isinstance(baseline, dict):
        paths = baseline.get("scope_paths")
        if isinstance(paths, list) and paths:
            return normalize_scope_paths([str(item) for item in paths if isinstance(item, str)])
    return normalize_scope_paths(fallback_paths or [])


def build_live_scope_packet(
    context: ToolExecutionContext,
    *,
    scope_paths: list[str] | None = None,
    recent_seconds: float = _DEFAULT_SCOPE_RECENT_SECONDS,
) -> dict[str, Any]:
    """Build the current live scope packet for *scope_paths*."""
    baseline = context.metadata.get("scope_packet")
    return build_scope_packet_for_context(
        context,
        scope_paths=scope_paths,
        baseline_packet=baseline if isinstance(baseline, dict) else None,
        recent_seconds=recent_seconds,
    )


def refresh_scope_baseline(
    context: ToolExecutionContext,
    *,
    scope_paths: list[str] | None = None,
    packet: dict[str, Any] | None = None,
    recent_seconds: float = _DEFAULT_SCOPE_RECENT_SECONDS,
) -> dict[str, Any]:
    """Persist the latest live scope packet into the tool metadata."""
    resolved = packet if isinstance(packet, dict) else build_live_scope_packet(
        context,
        scope_paths=scope_paths,
        recent_seconds=recent_seconds,
    )
    if not isinstance(resolved, dict):
        return {}
    context.metadata["scope_packet"] = resolved
    context.metadata["coherence_token"] = str(resolved.get("coherence_token") or "")
    return resolved


def enforce_scope_coherence(
    context: ToolExecutionContext,
    *,
    scope_paths: list[str] | None,
) -> tuple[dict[str, Any], str | None]:
    """Return the live scope packet plus an error when the baseline token drifted."""
    packet = build_live_scope_packet(context, scope_paths=scope_paths)
    expected = str(context.metadata.get("coherence_token") or "")
    current = str(packet.get("coherence_token") or "")
    if expected and current and expected != current:
        return packet, (
            "Scope coherence changed since the work item started. "
            "Refresh live CI state with ci_scope_status before writing."
        )
    return packet, None


def prepare_ci_write(
    context: ToolExecutionContext,
    file_path: str,
    *,
    expected_hash: str = "",
) -> tuple[Any | None, dict[str, Any], str | None]:
    """Run scope/token prechecks and reserve *file_path* for a write."""
    scope_paths = scope_paths_for_write(context, fallback_paths=[file_path])
    packet, err = enforce_scope_coherence(context, scope_paths=scope_paths)
    if err is not None:
        return None, packet, err
    svc = get_ci_service(context)
    if svc is None or not hasattr(svc, "prepare_write"):
        refresh_scope_baseline(context, packet=packet)
        return None, packet, None
    prepared = svc.prepare_write(
        file_path,
        agent_id=str(context.metadata.get("agent_run_id") or ""),
        expected_hash=expected_hash,
        allow_missing=True,
    )
    if getattr(prepared, "success", None) is False:
        return None, packet, str(getattr(prepared, "message", "") or "write precheck failed")
    refreshed = refresh_scope_baseline(context, scope_paths=scope_paths)
    return prepared, refreshed or packet, None


def finalize_ci_write(
    context: ToolExecutionContext,
    prepared: Any,
    *,
    content: str,
    edit_type: str,
    description: str,
) -> Any:
    """Commit a prepared write via the CI service."""
    svc = get_ci_service(context)
    assert svc is not None and hasattr(svc, "commit_prepared_write")
    result = svc.commit_prepared_write(
        prepared,
        content,
        edit_type=edit_type,
        description=description,
    )
    if getattr(result, "success", False):
        _note_atlas_edit(context, getattr(prepared, "file_path", ""), reason=edit_type)
        refresh_scope_baseline(
            context,
            scope_paths=scope_paths_for_write(
                context,
                fallback_paths=[getattr(prepared, "file_path", "")],
            ),
        )
    return result


def abort_ci_write(context: ToolExecutionContext, prepared: Any | None) -> None:
    """Release any prepared CI write reservation."""
    if prepared is None:
        return
    svc = get_ci_service(context)
    if svc is None or not hasattr(svc, "abort_prepared_write"):
        return
    try:
        svc.abort_prepared_write(prepared)
    except Exception:
        logger.debug("abort_prepared_write failed for %s", getattr(prepared, "file_path", ""), exc_info=True)
    finally:
        refresh_scope_baseline(
            context,
            scope_paths=scope_paths_for_write(
                context,
                fallback_paths=[getattr(prepared, "file_path", "")],
            ),
        )


def resolve_daytona_path(path: str, context: ToolExecutionContext) -> str:
    """Resolve *path* against the injected Daytona cwd."""
    if not path:
        return get_daytona_cwd(context) or "."
    if path.startswith("/"):
        return path
    cwd = get_daytona_cwd(context)
    if not cwd:
        return path
    return os.path.normpath(f"{cwd}/{path}")


def shell_mutation_declaration_error(
    context: ToolExecutionContext,
    *,
    command: str,
    declared_output_paths: list[str] | None,
) -> str | None:
    """Return an error when a mutating shell command lacks declared outputs."""
    if not require_declared_shell_outputs(context):
        return None
    if not command_may_mutate_workspace(command):
        return None
    if normalize_scope_paths(declared_output_paths or []):
        return None
    return (
        "Mutating daytona_bash calls must declare `declared_output_paths` in ultra "
        "coordination mode. Prefer daytona_write_file/daytona_edit_file, or list every "
        "path the command may create, modify, move, or delete before running it."
    )


def prime_cache_after_write(context: ToolExecutionContext, file_path: str, content: str) -> None:
    """Prime the tree cache and refresh the symbol index after a write."""
    svc = get_ci_service(context)
    if svc is None:
        _note_atlas_edit(context, file_path, reason="write")
        refresh_scope_baseline(
            context,
            scope_paths=scope_paths_for_write(context, fallback_paths=[file_path]),
        )
        return
    try:
        svc.tree_cache.put_content(file_path, content)
        svc.symbol_index.refresh(file_path, content)
        svc.lsp_client.invalidate(file_path)
    except Exception:
        logger.debug("CI prime_cache_after_write failed for %s", file_path)
    finally:
        _note_atlas_edit(context, file_path, reason="write")
        refresh_scope_baseline(
            context,
            scope_paths=scope_paths_for_write(context, fallback_paths=[file_path]),
        )


def sync_write_to_ci(
    context: ToolExecutionContext,
    file_path: str,
    content: str,
    *,
    agent_id: str = "",
    edit_type: str = "write",
    description: str = "",
    old_hash: str = "",
    new_hash: str = "",
) -> None:
    """Record a write in the ledger/arbiter and refresh CI caches."""
    svc = get_ci_service(context)
    if svc is not None:
        try:
            arbiter = getattr(svc, "arbiter", None)
            if arbiter is not None:
                arbiter.record_edit(file_path, agent_id)
        except Exception:
            logger.debug("CI arbiter sync failed for %s", file_path, exc_info=True)
    record_edit_in_ledger(
        context,
        file_path,
        agent_id=agent_id,
        edit_type=edit_type,
        old_hash=old_hash,
        new_hash=new_hash,
        description=description,
    )
    prime_cache_after_write(context, file_path, content)


def sync_deleted_file(
    context: ToolExecutionContext,
    file_path: str,
    *,
    agent_id: str = "",
    edit_type: str = "delete",
    description: str = "",
) -> None:
    """Best-effort CI invalidation for a deleted file."""
    svc = get_ci_service(context)
    if svc is not None:
        try:
            arbiter = getattr(svc, "arbiter", None)
            if arbiter is not None:
                arbiter.record_edit(file_path, agent_id)
        except Exception:
            logger.debug("CI arbiter delete sync failed for %s", file_path, exc_info=True)
        try:
            svc.tree_cache.invalidate(file_path)
            svc.symbol_index.refresh(file_path, "")
            svc.lsp_client.invalidate(file_path)
        except Exception:
            logger.debug("CI delete invalidation failed for %s", file_path, exc_info=True)
    record_edit_in_ledger(
        context,
        file_path,
        agent_id=agent_id,
        edit_type=edit_type,
        description=description,
    )
    _note_atlas_edit(context, file_path, reason=edit_type)
    refresh_scope_baseline(
        context,
        scope_paths=scope_paths_for_write(context, fallback_paths=[file_path]),
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
    keep CI caches, ledger, hotspots, and atlas invalidation in sync when an
    agent edits files via ``daytona_bash`` instead of structured edit tools.
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
    for file_path in dirty_paths[:limit]:
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
    }


def prepare_declared_shell_outputs(
    context: ToolExecutionContext,
    *,
    declared_output_paths: list[str] | None,
) -> tuple[list[Any], dict[str, Any], str | None]:
    """Reserve declared shell outputs before running a mutating command."""
    paths = normalize_scope_paths(declared_output_paths or [])
    packet, err = enforce_scope_coherence(context, scope_paths=paths)
    if err is not None:
        return [], packet, err
    if not paths:
        return [], packet, None
    prepared_items: list[Any] = []
    for path in paths:
        prepared, _, prep_err = prepare_ci_write(context, path)
        if prep_err is not None:
            for item in prepared_items:
                abort_ci_write(context, item)
            return [], packet, prep_err
        if prepared is not None:
            prepared_items.append(prepared)
    latest = context.metadata.get("scope_packet")
    return prepared_items, latest if isinstance(latest, dict) else packet, None


def release_declared_shell_outputs(context: ToolExecutionContext, prepared_items: list[Any]) -> None:
    """Release any declared shell reservations."""
    for item in prepared_items:
        abort_ci_write(context, item)
    if prepared_items:
        refresh_scope_baseline(
            context,
            scope_paths=normalize_scope_paths(
                [
                    str(getattr(item, "file_path", "") or "")
                    for item in prepared_items
                ]
            ),
        )


def record_edit_in_ledger(
    context: ToolExecutionContext,
    file_path: str,
    agent_id: str = "",
    edit_type: str = "edit",
    old_hash: str = "",
    new_hash: str = "",
    description: str = "",
) -> None:
    """Record an edit in the CI ledger if available."""
    svc = get_ci_service(context)
    if svc is None:
        return
    try:
        svc.ledger.record(
            file_path=file_path,
            agent_id=agent_id,
            edit_type=edit_type,
            old_hash=old_hash,
            new_hash=new_hash,
            description=description,
        )
    except Exception:
        logger.debug("CI record_edit_in_ledger failed for %s", file_path)


def _note_atlas_edit(
    context: ToolExecutionContext,
    file_path: str,
    *,
    reason: str,
) -> None:
    """Tell the live TeamRun that a file changed so atlas can refresh lazily."""
    team_run_id = context.metadata.get("team_run_id")
    if not team_run_id:
        return
    team_run = _get_team_run(str(team_run_id))
    if team_run is None:
        return
    try:
        team_run.note_atlas_edit(file_path, reason=reason)
    except Exception:
        logger.debug("atlas dirty-mark failed for %s", file_path, exc_info=True)


def _get_team_run(team_run_id: str) -> Any | None:
    try:
        from team.runtime.registry import get as get_team_run
    except Exception:
        return None
    try:
        return get_team_run(team_run_id)
    except Exception:
        return None


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

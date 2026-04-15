"""Shared code-intelligence runtime helpers used across toolkits."""

from __future__ import annotations

import copy
import dataclasses
import hashlib
import logging
from typing import Any

from code_intelligence.editing.merge import detect_edit_window
from tools.core.base import ToolExecutionContext

logger = logging.getLogger(__name__)


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


def get_ci_service(context: ToolExecutionContext) -> Any | None:
    """Get the CodeIntelligenceService from context, or None if unavailable."""
    return context.metadata.get("ci_service")


def _team_edit_ids(context: ToolExecutionContext) -> tuple[str, str, str]:
    return (
        str(context.metadata.get("team_run_id") or ""),
        str(context.metadata.get("agent_run_id") or ""),
        str(context.metadata.get("work_item_id") or ""),
    )


def _update_prepared_write(prepared: Any, **fields: Any) -> Any:
    """Return a shallow copy of *prepared* with updated fields."""
    if dataclasses.is_dataclass(prepared) and not isinstance(prepared, type):
        return dataclasses.replace(prepared, **fields)
    updated = copy.copy(prepared)
    for key, value in fields.items():
        setattr(updated, key, value)
    return updated


def _enrich_prepared_write_with_line_range(prepared: Any, content: str) -> Any:
    """Attach the minimal changed line range to *prepared* when possible."""
    current_content = str(getattr(prepared, "current_content", "") or "")
    line_start, line_end, operation_type = detect_edit_window(current_content, content)
    if line_start is None:
        return prepared
    return _update_prepared_write(
        prepared,
        line_start=line_start,
        line_end=line_end,
        operation_type=operation_type,
    )


def _find_enclosing_symbol(
    prepared: Any, context: ToolExecutionContext,
) -> tuple[str, int, int] | None:
    """Find the narrowest symbol boundary enclosing the prepared write's diff range."""
    line_start = getattr(prepared, "line_start", None)
    if line_start is None:
        return None
    svc = get_ci_service(context)
    symbol_index = getattr(svc, "symbol_index", None)
    file_path = str(getattr(prepared, "file_path", "") or "")
    if symbol_index is None or not file_path:
        return None
    try:
        boundaries = symbol_index.symbol_boundaries_for_file(file_path)
    except Exception:
        logger.debug("symbol_boundaries_for_file failed for %s", file_path, exc_info=True)
        return None
    if not isinstance(boundaries, list) or not boundaries:
        return None
    diff_start = int(line_start)
    diff_end = getattr(prepared, "line_end", None)
    diff_end = int(diff_end) if diff_end is not None else diff_start
    best: tuple[str, int, int] | None = None
    best_size: int | None = None
    for sym_name, sym_start, sym_end in boundaries:
        if sym_start <= diff_start and sym_end >= diff_end - 1:
            size = sym_end - sym_start
            if best is None or best_size is None or size < best_size:
                best = (sym_name, sym_start, sym_end)
                best_size = size
    return best


def _enrich_prepared_write_with_symbol_boundaries(
    prepared: Any, context: ToolExecutionContext
) -> Any:
    """Widen line anchors to the narrowest enclosing symbol when available."""
    best = _find_enclosing_symbol(prepared, context)
    if best is None:
        return prepared
    _, sym_start, sym_end = best
    return _update_prepared_write(prepared, line_start=sym_start, line_end=sym_end + 1)


def _intent_symbols_for_prepared_write(prepared: Any, context: ToolExecutionContext) -> list[str]:
    """Return the narrowest enclosing symbol for *prepared* when possible."""
    best = _find_enclosing_symbol(prepared, context)
    return [best[0]] if best is not None else []


def prepare_ci_edit_intent(
    context: ToolExecutionContext,
    prepared: Any,
    *,
    content: str,
) -> tuple[Any, str | None]:
    """Enrich *prepared* and publish an edit intent when the CI service supports it."""
    prepared = _enrich_prepared_write_with_line_range(prepared, content)
    prepared = _enrich_prepared_write_with_symbol_boundaries(prepared, context)

    svc = get_ci_service(context)
    publish = getattr(svc, "publish_edit_intent", None)
    if svc is None or type(svc).__module__ == "unittest.mock" or not callable(publish):
        return prepared, None

    symbols = _intent_symbols_for_prepared_write(prepared, context)
    scope = (
        "symbol"
        if symbols
        else ("line" if getattr(prepared, "line_start", None) is not None else "file")
    )
    try:
        intent_id = publish(
            filepath=str(getattr(prepared, "file_path", "") or ""),
            agent_id=str(context.metadata.get("agent_run_id") or ""),
            symbols=symbols or None,
            scope=scope,
        )
    except Exception:
        logger.debug(
            "publish_edit_intent failed for %s", getattr(prepared, "file_path", ""), exc_info=True
        )
        return prepared, None

    heartbeat = getattr(svc, "heartbeat_edit_intent", None)
    if callable(heartbeat):
        try:
            heartbeat(intent_id)
        except Exception:
            logger.debug("heartbeat_edit_intent failed for %s", intent_id, exc_info=True)
    return prepared, intent_id


def release_ci_edit_intent(context: ToolExecutionContext, intent_id: str | None) -> None:
    """Release an edit intent when the CI service supports it."""
    if not intent_id:
        return
    svc = get_ci_service(context)
    release = getattr(svc, "release_edit_intent", None) if svc is not None else None
    if not callable(release):
        return
    try:
        release(intent_id)
    except Exception:
        logger.debug("release_edit_intent failed for %s", intent_id, exc_info=True)


def prepare_ci_write(
    context: ToolExecutionContext,
    file_path: str,
    *,
    expected_hash: str = "",
    allow_scope_drift: bool = False,
) -> tuple[Any | None, dict[str, Any], str | None]:
    """Run prechecks and reserve *file_path* for a write."""
    svc = get_ci_service(context)
    if svc is None or not hasattr(svc, "prepare_write"):
        return None, {}, None
    prepared = svc.prepare_write(
        file_path,
        agent_id=str(context.metadata.get("agent_run_id") or ""),
        expected_hash=expected_hash,
        allow_missing=True,
    )
    if getattr(prepared, "success", None) is False:
        message = str(getattr(prepared, "message", "") or "write precheck failed")
        _note_team_memory_conflict(
            context,
            file_path=file_path,
            reason=message,
        )
        return None, {}, message
    return prepared, {}, None


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
    prepared = _enrich_prepared_write_with_line_range(prepared, content)
    prepared = _enrich_prepared_write_with_symbol_boundaries(prepared, context)
    result = svc.commit_prepared_write(
        prepared,
        content,
        edit_type=edit_type,
        description=description,
    )
    _finalize_ci_commit_result(
        context,
        result=result,
        file_path=str(getattr(prepared, "file_path", "") or ""),
        edit_type=edit_type,
        old_hash=str(getattr(prepared, "current_hash", "") or ""),
        new_hash=_content_hash(content),
        description=description,
        ci_arbiter=getattr(svc, "arbiter", None),
    )
    return result


def commit_ci_change_against_base(
    context: ToolExecutionContext,
    file_path: str,
    *,
    base_content: str | None,
    final_content: str | None,
    edit_type: str,
    description: str,
) -> Any:
    """Commit a file change against an explicit base snapshot via the CI service."""
    svc = get_ci_service(context)
    assert svc is not None and hasattr(svc, "commit_change_against_base")
    result = svc.commit_change_against_base(
        file_path,
        base_content=base_content,
        final_content=final_content,
        agent_id=_resolved_agent_id(context),
        edit_type=edit_type,
        description=description,
    )
    _finalize_ci_commit_result(
        context,
        result=result,
        file_path=file_path,
        edit_type=edit_type,
        old_hash=_content_hash(base_content or "") if base_content is not None else "",
        new_hash=_content_hash(final_content) if final_content is not None else "",
        description=description,
        ci_arbiter=getattr(svc, "arbiter", None),
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
        logger.debug(
            "abort_prepared_write failed for %s", getattr(prepared, "file_path", ""), exc_info=True
        )


def prime_cache_after_write(context: ToolExecutionContext, file_path: str, content: str) -> None:
    """Prime the tree cache and refresh the symbol index after a write."""
    svc = get_ci_service(context)
    if svc is None:
        return
    try:
        svc.symbol_index.refresh(file_path, content)
        svc.lsp_client.invalidate(file_path)
    except Exception:
        logger.debug("CI prime_cache_after_write failed for %s", file_path)


def record_edit_in_arbiter(
    context: ToolExecutionContext,
    file_path: str,
    *,
    agent_id: str = "",
    edit_type: str = "edit",
    old_hash: str = "",
    new_hash: str = "",
    description: str = "",
) -> None:
    """Record an edit in the CI arbiter if available."""
    svc = get_ci_service(context)
    if svc is None:
        return
    team_run_id, agent_run_id, task_id = _team_edit_ids(context)
    try:
        arbiter = getattr(svc, "arbiter", None)
        if arbiter is not None:
            arbiter.record_edit(
                file_path=file_path,
                team_run_id=team_run_id,
                agent_run_id=agent_run_id,
                task_id=task_id,
                edit_type=edit_type,
                old_hash=old_hash,
                new_hash=new_hash,
                description=description,
            )
    except Exception:
        logger.debug("CI arbiter sync failed for %s", file_path, exc_info=True)
    _propagate_team_edit(
        context,
        file_path=file_path,
        agent_run_id=agent_run_id,
        task_id=task_id,
        edit_type=edit_type,
        old_hash=old_hash,
        new_hash=new_hash,
        description=description,
        ci_arbiter=getattr(svc, "arbiter", None),
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
    """Record a write in the arbiter and refresh CI caches."""
    record_edit_in_arbiter(
        context, file_path, agent_id=agent_id, edit_type=edit_type,
        old_hash=old_hash, new_hash=new_hash, description=description,
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
    record_edit_in_arbiter(
        context, file_path, agent_id=agent_id,
        edit_type=edit_type, description=description,
    )
    svc = get_ci_service(context)
    if svc is not None:
        try:
            svc.symbol_index.refresh(file_path, "")
            svc.lsp_client.invalidate(file_path)
        except Exception:
            logger.debug("CI delete invalidation failed for %s", file_path, exc_info=True)



def _note_team_memory_conflict(
    context: ToolExecutionContext,
    *,
    file_path: str,
    reason: str,
) -> None:
    """Persist a typed conflict event when a TeamRun is active."""
    team_run_id = context.metadata.get("team_run_id")
    if not team_run_id:
        return
    team_run = _get_team_run(str(team_run_id))
    if team_run is None or not hasattr(team_run, "note_conflict_event"):
        return
    try:
        team_run.note_conflict_event(
            file_path=file_path,
            reason=reason,
            work_item_id=str(context.metadata.get("work_item_id") or ""),
            agent_name=str(context.metadata.get("agent_name") or ""),
        )
    except Exception:
        logger.debug("team memory conflict persistence failed for %s", file_path, exc_info=True)


def _resolved_agent_id(context: ToolExecutionContext, *, preferred: str = "") -> str:
    agent_id = str(preferred or "").strip()
    if agent_id:
        return agent_id
    agent_name = str(context.metadata.get("agent_name") or "").strip()
    if agent_name:
        return agent_name
    return str(context.metadata.get("agent_run_id") or "").strip()


def _finalize_ci_commit_result(
    context: ToolExecutionContext,
    *,
    result: Any,
    file_path: str,
    edit_type: str,
    old_hash: str,
    new_hash: str,
    description: str,
    ci_arbiter: Any | None,
) -> None:
    if bool(getattr(result, "success", False)):
        _, agent_run_id, task_id = _team_edit_ids(context)
        _propagate_team_edit(
            context,
            file_path=file_path,
            agent_run_id=agent_run_id,
            task_id=task_id,
            edit_type=edit_type,
            old_hash=old_hash,
            new_hash=new_hash,
            description=description,
            ci_arbiter=ci_arbiter,
        )
        return
    if bool(getattr(result, "conflict", False)):
        _note_team_memory_conflict(
            context,
            file_path=file_path,
            reason=str(
                getattr(result, "conflict_reason", "")
                or getattr(result, "message", "")
                or "write conflict"
            ),
        )


def _propagate_team_edit(
    context: ToolExecutionContext,
    *,
    file_path: str,
    agent_run_id: str,
    task_id: str,
    edit_type: str,
    old_hash: str,
    new_hash: str,
    description: str,
    ci_arbiter: Any | None,
) -> None:
    """Mirror successful edits into the team-run coordination stream."""
    team_run_id = str(context.metadata.get("team_run_id") or "")
    if not team_run_id or not file_path:
        return
    team_run = _get_team_run(team_run_id)
    if team_run is None:
        return

    store = getattr(team_run, "arbiter", None)
    if (
        store is not None
        and getattr(store, "initialized", False)
        and store is not ci_arbiter
    ):
        try:
            store.record_edit(
                file_path=file_path,
                team_run_id=team_run_id,
                agent_run_id=agent_run_id,
                task_id=task_id,
                edit_type=edit_type,
                old_hash=old_hash,
                new_hash=new_hash,
                description=description,
            )
        except Exception:
            logger.debug("team arbiter mirror failed for %s", file_path, exc_info=True)



def _get_team_run(team_run_id: str) -> Any | None:
    try:
        from team.runtime.registry import get as get_team_run
    except Exception:
        return None
    try:
        return get_team_run(team_run_id)
    except Exception:
        return None


__all__ = [
    "abort_ci_write",
    "commit_ci_change_against_base",
    "finalize_ci_write",
    "get_ci_service",
    "prepare_ci_edit_intent",
    "prepare_ci_write",
    "prime_cache_after_write",
    "record_edit_in_arbiter",
    "release_ci_edit_intent",
    "sync_deleted_file",
    "sync_write_to_ci",
]

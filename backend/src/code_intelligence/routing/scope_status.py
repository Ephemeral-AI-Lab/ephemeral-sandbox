"""Scope coordination snapshots for code intelligence services."""

from __future__ import annotations

import time
from typing import Any

from team.core.scope import normalize_scope_paths, scope_paths_overlap

from code_intelligence.tuning import CODE_INTELLIGENCE_TUNING


def build_scope_status(
    *,
    arbiter: Any,
    symbol_index: Any,
    scope_paths: list[str] | tuple[str, ...] | None,
    team_run_id: str | None = None,
    briefing_versions: list[dict[str, Any]] | None = None,
    context_pressure: dict[str, Any] | None = None,
    shared_context: list[dict[str, Any]] | None = None,
    baseline_packet: dict[str, Any] | None = None,
    recent_seconds: float = CODE_INTELLIGENCE_TUNING.scope_recent_seconds,
) -> dict[str, Any]:
    """Return the authoritative live coordination snapshot for *scope_paths*."""
    del briefing_versions, context_pressure, shared_context, baseline_packet
    normalized = normalize_scope_paths(scope_paths)
    history_ready = getattr(arbiter, "initialized", False)

    recent_changes: list[dict[str, Any]] = []
    if history_ready:
        for entry in arbiter.recent_edits(seconds=recent_seconds, team_run_id=team_run_id):
            fp = str(entry.file_path or "")
            if _scope_excludes(fp, normalized):
                continue
            recent_changes.append(
                {
                    "file_path": fp,
                    "agent_run_id": str(entry.agent_run_id or ""),
                    "task_id": str(entry.task_id or ""),
                    "timestamp": entry.created_at.timestamp() if entry.created_at else 0.0,
                    "edit_type": str(entry.edit_type or ""),
                }
            )
    recent_changes.sort(key=lambda item: (item["file_path"], item["timestamp"]))

    hotspots: list[dict[str, Any]] = []
    if history_ready:
        for fp, count in arbiter.hotspots(limit=25, team_run_id=team_run_id):
            fp_str = str(fp)
            if _scope_excludes(fp_str, normalized):
                continue
            hotspots.append({"file_path": fp_str, "edit_count": int(count)})
            if len(hotspots) >= 10:
                break

    return {
        "scope_paths": normalized,
        "arbiter_generation": arbiter.generation,
        "symbol_index_generation": symbol_index.generation,
        "recent_changes": recent_changes[:25],
        "hotspots": hotspots,
        "generated_at": time.time(),
    }


def _scope_excludes(file_path: str, normalized_scope: list[str]) -> bool:
    """True if *normalized_scope* is non-empty and *file_path* does not overlap any entry."""
    if not normalized_scope:
        return False
    return not any(scope_paths_overlap(file_path, scope) for scope in normalized_scope)

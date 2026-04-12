"""Scope packet builder for write coordination and prompt injection.

Builds machine-checkable live scope packets by gathering state from
the CI service, team run, ledger, and arbiter.  Migrated from
tools.daytona_toolkit.coordination.
"""

from __future__ import annotations

import time
from typing import Any

from code_intelligence.routing.scope_packets import (
    build_scope_packet as build_shared_scope_packet,
    normalize_scope_paths,
    scope_paths_overlap,
)
from tools.core.base import ToolExecutionContext

_DEFAULT_RECENT_SECONDS = 300.0


def build_scope_packet(
    *,
    scope_paths: list[str] | tuple[str, ...] | None,
    svc: Any | None = None,
    team_run: Any | None = None,
    baseline_packet: dict[str, Any] | None = None,
    recent_seconds: float = _DEFAULT_RECENT_SECONDS,
) -> dict[str, Any]:
    """Build a machine-checkable live scope packet."""
    normalized = normalize_scope_paths(scope_paths)
    briefing_versions = _matching_briefing_versions(team_run, normalized)
    context_pressure: dict[str, Any] = {}
    shared_context = ""
    scope_status = getattr(svc, "scope_status", None)
    if callable(scope_status):
        try:
            packet = scope_status(
                normalized,
                briefing_versions=briefing_versions,
                context_pressure=context_pressure,
                shared_context=shared_context,
                baseline_packet=baseline_packet,
                recent_seconds=recent_seconds,
            )
        except Exception:
            packet = None
        if isinstance(packet, dict):
            return packet
    return build_shared_scope_packet(
        scope_paths=normalized,
        briefing_versions=briefing_versions,
        ledger_generation=_safe_generation(getattr(svc, "ledger", None)),
        arbiter_generation=_safe_generation(getattr(svc, "arbiter", None)),
        symbol_index_generation=_safe_generation(getattr(svc, "symbol_index", None)),
        recent_changes=_recent_changes(svc, normalized, seconds=recent_seconds),
        active_reservations=_active_reservations(svc, normalized),
        active_edit_intents=_active_edit_intents(svc, normalized),
        hotspots=_hotspots(svc, normalized),
        context_pressure=context_pressure,
        shared_context=shared_context,
        generated_at=time.time(),
        baseline_packet=baseline_packet,
    )


def build_scope_packet_for_context(
    context: ToolExecutionContext,
    *,
    scope_paths: list[str] | tuple[str, ...] | None = None,
    baseline_packet: dict[str, Any] | None = None,
    recent_seconds: float = _DEFAULT_RECENT_SECONDS,
) -> dict[str, Any]:
    """Build a scope packet using the current tool context."""
    svc = context.metadata.get("ci_service")
    team_run = None
    team_run_id = context.metadata.get("team_run_id")
    if isinstance(team_run_id, str) and team_run_id:
        team_run = _get_team_run(team_run_id)
    if scope_paths is None:
        baseline = baseline_packet or context.metadata.get("scope_packet")
        if isinstance(baseline, dict):
            scope_paths = baseline.get("scope_paths") or []
    return build_scope_packet(
        scope_paths=scope_paths,
        svc=svc,
        team_run=team_run,
        baseline_packet=baseline_packet,
        recent_seconds=recent_seconds,
    )


def render_scope_packet(packet: dict[str, Any] | None) -> str:
    """Render a compact prompt preamble for a live scope packet."""
    if not isinstance(packet, dict):
        return ""
    scope_paths = ", ".join(packet.get("scope_paths") or []) or "(unscoped)"
    changes = ", ".join(item["file_path"] for item in (packet.get("recent_changes") or [])[:4]) or "none"
    reservations = ", ".join(item["file_path"] for item in (packet.get("active_reservations") or [])[:4]) or "none"
    admission = packet.get("admission") if isinstance(packet.get("admission"), dict) else {}
    admission_mode = str(admission.get("mode") or "unknown")
    reasons = "; ".join(str(item) for item in (admission.get("reasons") or []) if str(item).strip()) or "none"
    context_pressure = packet.get("context_pressure") if isinstance(packet.get("context_pressure"), dict) else {}
    context_score = float(context_pressure.get("score") or 0.0)
    context_level = str(context_pressure.get("level") or "low")
    context_reasons = "; ".join(
        str(item) for item in (context_pressure.get("reasons") or []) if str(item).strip()
    ) or "none"
    shared_context = packet.get("shared_context") if isinstance(packet.get("shared_context"), list) else []
    shared_summary = "; ".join(
        f"{item.get('scope')}:{item.get('kind')}/{item.get('freshness')}"
        for item in shared_context[:4]
        if isinstance(item, dict)
    ) or "none"
    return (
        "## Live scope packet\n"
        f"- freshness: {packet.get('freshness')}\n"
        f"- coherence_token: {packet.get('coherence_token')}\n"
        f"- scope_paths: {scope_paths}\n"
        f"- recent_changes: {changes}\n"
        f"- active_reservations: {reservations}\n"
        f"- context_pressure: {context_level} ({context_score:.2f})\n"
        f"- context_pressure_reasons: {context_reasons}\n"
        f"- shared_context: {shared_summary}\n"
        f"- scout_fanout_mode: {admission_mode}\n"
        f"- scout_fanout_reasons: {reasons}"
    )


# ---------------------------------------------------------------------------
# Private helpers
# ---------------------------------------------------------------------------


def _get_team_run(team_run_id: str) -> Any | None:
    try:
        from team.runtime.registry import get as get_team_run
    except Exception:
        return None
    try:
        return get_team_run(team_run_id)
    except Exception:
        return None


def _matching_briefing_versions(team_run: Any | None, scope_paths: list[str]) -> list[dict[str, Any]]:
    if team_run is None:
        return []
    project_context = getattr(team_run, "project_context", None)
    versions = getattr(project_context, "stable_scout_versions", {}) or {}
    out: list[dict[str, Any]] = []
    for scope, version in versions.items():
        if scope_paths and not any(
            scope_paths_overlap(scope_part, target)
            for scope_part in normalize_scope_paths([scope])
            for target in scope_paths
        ):
            continue
        if not isinstance(version, dict):
            continue
        out.append(
            {
                "scope": scope,
                "snapshot_time": float(version.get("snapshot_time") or 0.0),
                "run_id": str(version.get("run_id") or ""),
            }
        )
    out.sort(key=lambda item: item["scope"])
    return out


def _recent_changes(svc: Any | None, scope_paths: list[str], *, seconds: float) -> list[dict[str, Any]]:
    ledger = getattr(svc, "ledger", None)
    if ledger is None:
        return []
    try:
        entries = ledger.recent_entries(seconds)
    except Exception:
        return []
    out: list[dict[str, Any]] = []
    for entry in entries:
        file_path = str(getattr(entry, "file_path", "") or "")
        if scope_paths and not any(scope_paths_overlap(file_path, scope) for scope in scope_paths):
            continue
        out.append(
            {
                "file_path": file_path,
                "agent_id": str(getattr(entry, "agent_id", "") or ""),
                "timestamp": float(getattr(entry, "timestamp", 0.0) or 0.0),
                "edit_type": str(getattr(entry, "edit_type", "") or ""),
            }
        )
    out.sort(key=lambda item: (item["file_path"], item["timestamp"]))
    return out[:25]


def _active_reservations(svc: Any | None, scope_paths: list[str]) -> list[dict[str, Any]]:
    arbiter = getattr(svc, "arbiter", None)
    if arbiter is None or not hasattr(arbiter, "active_reservations"):
        return []
    try:
        reservations = arbiter.active_reservations(scope_paths)
    except Exception:
        return []
    return [dict(item) for item in reservations][:25]


def _hotspots(svc: Any | None, scope_paths: list[str]) -> list[dict[str, Any]]:
    arbiter = getattr(svc, "arbiter", None)
    if arbiter is None:
        return []
    try:
        hotspots = arbiter.hotspots(limit=25)
    except Exception:
        return []
    out = [
        {"file_path": str(file_path), "edit_count": int(count)}
        for file_path, count in hotspots
        if not scope_paths or any(scope_paths_overlap(str(file_path), scope) for scope in scope_paths)
    ]
    return out[:10]


def _active_edit_intents(svc: Any | None, scope_paths: list[str]) -> list[dict[str, Any]]:
    arbiter = getattr(svc, "arbiter", None)
    if arbiter is None or not hasattr(arbiter, "active_edit_intents"):
        return []
    try:
        intents = arbiter.active_edit_intents(scope_paths)
    except Exception:
        return []
    return [dict(item) for item in intents][:25]


def _safe_generation(obj: Any) -> int:
    raw = getattr(obj, "generation", 0)
    return int(raw) if isinstance(raw, (int, float)) else 0

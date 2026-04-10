"""Live scope packet helpers for write coordination and prompt injection."""

from __future__ import annotations

import hashlib
import json
import time
from typing import Any

from team.context.canonicalize import scope_of_artifact
from tools.core.base import ToolExecutionContext

_DEFAULT_RECENT_SECONDS = 300.0


def normalize_scope_paths(paths: list[str] | tuple[str, ...] | None) -> list[str]:
    """Return stable, deduplicated scope paths."""
    out: list[str] = []
    seen: set[str] = set()
    for raw in paths or ():
        if not isinstance(raw, str):
            continue
        for part in raw.split("|"):
            cleaned = part.strip().replace("\\", "/").removeprefix("./").rstrip("/")
            if not cleaned or cleaned in seen:
                continue
            seen.add(cleaned)
            out.append(cleaned)
    out.sort()
    return out


def scopes_overlap(path_a: str, path_b: str) -> bool:
    """Return True when two file or directory scopes overlap."""
    left = (path_a or "").strip().rstrip("/")
    right = (path_b or "").strip().rstrip("/")
    if not left or not right:
        return False
    if left == right:
        return True
    if left.startswith(right + "/") or right.startswith(left + "/"):
        return True
    return (
        left.endswith("/" + right)
        or right.endswith("/" + left)
        or ("/" + right + "/") in (left + "/")
        or ("/" + left + "/") in (right + "/")
    )


def scope_paths_from_payload(payload: Any) -> list[str]:
    """Extract the most likely scope paths from a work-item payload."""
    if not isinstance(payload, dict):
        return []
    collected: list[str] = []
    for key in ("touches_paths", "target_paths", "stale_subsystems", "paths", "files"):
        raw = payload.get(key)
        if isinstance(raw, list):
            collected.extend(str(item) for item in raw if isinstance(item, str))
    for key in ("file_path", "path", "subsystem", "canonical_scope"):
        raw = payload.get(key)
        if isinstance(raw, str) and raw.strip():
            collected.append(raw)
    return normalize_scope_paths(collected)


def scope_paths_for_work_item(team_run: Any, wi: Any) -> list[str]:
    """Resolve a work item's owned scope from payload plus attached artifacts."""
    candidates = scope_paths_from_payload(getattr(wi, "payload", None))
    if candidates:
        return candidates

    artifact_store = getattr(team_run, "artifacts", None)
    for dep in getattr(wi, "dep_artifacts", []) or ():
        if artifact_store is None:
            continue
        body = artifact_store.load(dep.artifact_ref)
        scope = scope_of_artifact(body)
        if scope:
            candidates.append(scope)

    for briefing in getattr(wi, "briefings", []) or ():
        if getattr(briefing, "source", "") == "artifact" and getattr(briefing, "ref", None) and artifact_store is not None:
            body = artifact_store.load(briefing.ref)
            scope = scope_of_artifact(body)
            if scope:
                candidates.append(scope)

    return normalize_scope_paths(candidates)


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
    scope_status = getattr(svc, "scope_status", None)
    if callable(scope_status):
        try:
            packet = scope_status(
                normalized,
                briefing_versions=briefing_versions,
                baseline_packet=baseline_packet,
                recent_seconds=recent_seconds,
            )
        except Exception:
            packet = None
        if isinstance(packet, dict):
            return packet
    recent_changes = _recent_changes(svc, normalized, seconds=recent_seconds)
    active_reservations = _active_reservations(svc, normalized)
    hotspots = _hotspots(svc, normalized)
    ledger_generation = _safe_attr(getattr(svc, "ledger", None), "generation")
    arbiter_generation = _safe_attr(getattr(svc, "arbiter", None), "generation")
    symbol_generation = _safe_attr(getattr(svc, "symbol_index", None), "generation")
    packet = {
        "scope_paths": normalized,
        "briefing_versions": briefing_versions,
        "ledger_generation": ledger_generation,
        "arbiter_generation": arbiter_generation,
        "symbol_index_generation": symbol_generation,
        "recent_changes": recent_changes,
        "active_reservations": active_reservations,
        "hotspots": hotspots,
        "generated_at": time.time(),
    }
    packet["coherence_token"] = _coherence_token(packet)
    packet["freshness"] = _freshness_grade(packet, baseline_packet)
    if isinstance(baseline_packet, dict):
        packet["baseline_coherence_token"] = str(baseline_packet.get("coherence_token") or "")
    packet["admission"] = _admission(packet)
    return packet


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


def _get_team_run(team_run_id: str) -> Any | None:
    try:
        from team.runtime.registry import get as get_team_run
    except Exception:
        return None
    try:
        return get_team_run(team_run_id)
    except Exception:
        return None


def render_scope_packet(packet: dict[str, Any] | None) -> str:
    """Render a compact prompt preamble for a live scope packet."""
    if not isinstance(packet, dict):
        return ""
    scope_paths = ", ".join(packet.get("scope_paths") or []) or "(unscoped)"
    changes = ", ".join(item["file_path"] for item in (packet.get("recent_changes") or [])[:4]) or "none"
    reservations = ", ".join(item["file_path"] for item in (packet.get("active_reservations") or [])[:4]) or "none"
    admission = packet.get("admission") if isinstance(packet.get("admission"), dict) else {}
    admission_mode = str(admission.get("mode") or "unknown")
    scout_budget = int(admission.get("recommended_parallel_scouts") or 0)
    return (
        "## Live scope packet\n"
        f"- freshness: {packet.get('freshness')}\n"
        f"- coherence_token: {packet.get('coherence_token')}\n"
        f"- scope_paths: {scope_paths}\n"
        f"- recent_changes: {changes}\n"
        f"- active_reservations: {reservations}\n"
        f"- scout_fanout_mode: {admission_mode}\n"
        f"- recommended_parallel_scouts: {scout_budget}"
    )


def _matching_briefing_versions(team_run: Any | None, scope_paths: list[str]) -> list[dict[str, Any]]:
    if team_run is None:
        return []
    project_context = getattr(team_run, "project_context", None)
    versions = getattr(project_context, "stable_scout_versions", {}) or {}
    out: list[dict[str, Any]] = []
    for scope, version in versions.items():
        if scope_paths and not any(scopes_overlap(scope_part, target) for scope_part in normalize_scope_paths([scope]) for target in scope_paths):
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
        if scope_paths and not any(scopes_overlap(file_path, scope) for scope in scope_paths):
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
        if not scope_paths or any(scopes_overlap(str(file_path), scope) for scope in scope_paths)
    ]
    return out[:10]


def _coherence_token(packet: dict[str, Any]) -> str:
    stable = {
        "scope_paths": packet.get("scope_paths") or [],
        "briefing_versions": packet.get("briefing_versions") or [],
        "recent_changes": packet.get("recent_changes") or [],
        "active_reservations": packet.get("active_reservations") or [],
    }
    raw = json.dumps(stable, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()[:24]


def _freshness_grade(current: dict[str, Any], baseline_packet: dict[str, Any] | None) -> str:
    if isinstance(baseline_packet, dict) and normalize_scope_paths(current.get("scope_paths") or []) == normalize_scope_paths(
        baseline_packet.get("scope_paths") or []
    ):
        if str(current.get("coherence_token") or "") == str(baseline_packet.get("coherence_token") or ""):
            return "fresh"
        if current.get("active_reservations") or current.get("recent_changes"):
            return "touched"
        return "stale"
    if current.get("active_reservations") or current.get("recent_changes"):
        return "touched"
    return "fresh"
def _admission(packet: dict[str, Any]) -> dict[str, Any]:
    reservations = list(packet.get("active_reservations") or [])
    recent_changes = list(packet.get("recent_changes") or [])
    hotspots = list(packet.get("hotspots") or [])
    hotspot_max = max((int(item.get("edit_count") or 0) for item in hotspots), default=0)
    if reservations:
        return {
            "mode": "serialize",
            "contention": "high",
            "recommended_parallel_scouts": 1,
            "allow_parallel_fanout": False,
            "active_reservation_count": len(reservations),
            "recent_change_count": len(recent_changes),
            "hotspot_max_edit_count": hotspot_max,
            "reasons": ["active write reservations overlap this scope"],
        }
    if hotspot_max >= 4 or len(recent_changes) >= 6:
        return {
            "mode": "serialize",
            "contention": "high",
            "recommended_parallel_scouts": 1,
            "allow_parallel_fanout": False,
            "active_reservation_count": 0,
            "recent_change_count": len(recent_changes),
            "hotspot_max_edit_count": hotspot_max,
            "reasons": ["scope is in a high-churn hotspot window"],
        }
    if hotspot_max >= 2 or len(recent_changes) >= 2:
        return {
            "mode": "cautious",
            "contention": "medium",
            "recommended_parallel_scouts": 2,
            "allow_parallel_fanout": True,
            "active_reservation_count": 0,
            "recent_change_count": len(recent_changes),
            "hotspot_max_edit_count": hotspot_max,
            "reasons": ["scope changed recently; keep scout fanout narrow and disjoint"],
        }
    return {
        "mode": "parallel",
        "contention": "low",
        "recommended_parallel_scouts": 3,
        "allow_parallel_fanout": True,
        "active_reservation_count": 0,
        "recent_change_count": len(recent_changes),
        "hotspot_max_edit_count": hotspot_max,
        "reasons": ["scope is stable enough for disjoint scout fanout"],
    }


def _safe_attr(obj: Any, name: str) -> int:
    raw = getattr(obj, name, 0)
    return int(raw) if isinstance(raw, (int, float)) else 0

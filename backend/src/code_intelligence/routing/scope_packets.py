from __future__ import annotations

import hashlib
import json
from typing import Any


def normalize_scope_paths(paths: list[str] | tuple[str, ...] | None) -> list[str]:
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


def scope_paths_overlap(path_a: str, path_b: str) -> bool:
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


def stable_briefing_versions(value: list[dict[str, Any]] | None) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for item in value or []:
        if not isinstance(item, dict):
            continue
        out.append(
            {
                "scope": str(item.get("scope") or ""),
                "snapshot_time": float(item.get("snapshot_time") or 0.0),
                "run_id": str(item.get("run_id") or ""),
            }
        )
    out.sort(key=lambda entry: entry["scope"])
    return out


def stable_shared_context(value: list[dict[str, Any]] | None) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for item in value or []:
        if not isinstance(item, dict):
            continue
        out.append(
            {
                "scope": str(item.get("scope") or ""),
                "kind": str(item.get("kind") or ""),
                "provenance": str(item.get("provenance") or ""),
                "freshness": str(item.get("freshness") or ""),
                "consumer_count": int(item.get("consumer_count") or 0),
                "render_count": int(item.get("render_count") or 0),
                "scope_write_epoch": int(item.get("scope_write_epoch") or 0),
            }
        )
    out.sort(key=lambda entry: entry["scope"])
    return out


def build_scope_packet(
    *,
    scope_paths: list[str] | tuple[str, ...] | None,
    briefing_versions: list[dict[str, Any]] | None = None,
    ledger_generation: int = 0,
    arbiter_generation: int = 0,
    symbol_index_generation: int = 0,
    recent_changes: list[dict[str, Any]] | None = None,
    active_reservations: list[dict[str, Any]] | None = None,
    active_edit_intents: list[dict[str, Any]] | None = None,
    hotspots: list[dict[str, Any]] | None = None,
    context_pressure: dict[str, Any] | None = None,
    shared_context: list[dict[str, Any]] | None = None,
    generated_at: float | None = None,
    baseline_packet: dict[str, Any] | None = None,
) -> dict[str, Any]:
    packet = {
        "scope_paths": normalize_scope_paths(scope_paths),
        "briefing_versions": stable_briefing_versions(briefing_versions),
        "ledger_generation": int(ledger_generation),
        "arbiter_generation": int(arbiter_generation),
        "symbol_index_generation": int(symbol_index_generation),
        "recent_changes": list(recent_changes or []),
        "active_reservations": list(active_reservations or []),
        "active_edit_intents": list(active_edit_intents or []),
        "hotspots": list(hotspots or []),
        "context_pressure": dict(context_pressure or {}),
        "shared_context": stable_shared_context(shared_context),
        "generated_at": generated_at,
    }
    packet["coherence_token"] = scope_coherence_token(packet)
    packet["freshness"] = scope_freshness(packet, baseline_packet)
    if isinstance(baseline_packet, dict):
        packet["baseline_coherence_token"] = str(baseline_packet.get("coherence_token") or "")
    packet["admission"] = scope_admission(packet)
    return packet


def scope_coherence_token(packet: dict[str, Any]) -> str:
    stable = {
        "scope_paths": packet.get("scope_paths") or [],
        "briefing_versions": packet.get("briefing_versions") or [],
        "recent_changes": packet.get("recent_changes") or [],
        "active_reservations": packet.get("active_reservations") or [],
        "active_edit_intents": packet.get("active_edit_intents") or [],
        "context_pressure": packet.get("context_pressure") or {},
        "shared_context": packet.get("shared_context") or [],
    }
    encoded = json.dumps(stable, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()[:24]


def same_scope(current: dict[str, Any], baseline_packet: dict[str, Any] | None) -> bool:
    if not isinstance(baseline_packet, dict):
        return False
    return normalize_scope_paths(current.get("scope_paths") or []) == normalize_scope_paths(
        baseline_packet.get("scope_paths") or []
    )


def scope_freshness(current: dict[str, Any], baseline_packet: dict[str, Any] | None) -> str:
    if same_scope(current, baseline_packet):
        if str(current.get("coherence_token") or "") == str(baseline_packet.get("coherence_token") or ""):
            return "fresh"
        if current.get("active_reservations") or current.get("recent_changes") or current.get("active_edit_intents"):
            return "touched"
        return "stale"
    if current.get("active_reservations") or current.get("recent_changes") or current.get("active_edit_intents"):
        return "touched"
    return "fresh"


def scope_admission(packet: dict[str, Any]) -> dict[str, Any]:
    reservations = list(packet.get("active_reservations") or [])
    intents = list(packet.get("active_edit_intents") or [])
    recent_changes = list(packet.get("recent_changes") or [])
    hotspots = list(packet.get("hotspots") or [])
    hotspot_max = max((int(item.get("edit_count") or 0) for item in hotspots), default=0)
    change_count = len(recent_changes)
    reasons: list[str] = []

    if reservations:
        mode = "serialize"
        contention = "high"
        reasons.append("active write reservations overlap this scope")
    elif hotspot_max >= 4 or change_count >= 6:
        mode = "serialize"
        contention = "high"
        reasons.append("scope is in a high-churn hotspot window")
    elif hotspot_max >= 2 or change_count >= 2:
        mode = "cautious"
        contention = "medium"
        reasons.append("scope changed recently; keep scout fanout narrow and disjoint")
    elif intents:
        mode = "cautious"
        contention = "medium"
        reasons.append("active edit intents exist in this scope; prefer disjoint work")
    else:
        mode = "parallel"
        contention = "low"
        reasons.append("scope is stable enough for disjoint scout fanout")

    return {
        "mode": mode,
        "contention": contention,
        "allow_parallel_fanout": mode != "serialize",
        "active_reservation_count": len(reservations),
        "recent_change_count": change_count,
        "hotspot_max_edit_count": hotspot_max,
        "reasons": reasons,
    }

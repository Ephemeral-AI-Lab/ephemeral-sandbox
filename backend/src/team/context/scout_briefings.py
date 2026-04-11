"""Stable scout artifact storage and same-run auto-promotion helpers."""

from __future__ import annotations

import logging
import time
from types import SimpleNamespace
from typing import Any

from code_intelligence.atlas.freshness import (
    MIN_COMPLETE_SCOPE_COVERAGE,
    brief_reuse_status,
    freshness_status,
)
from team.context.canonicalize import canonicalize_scope, scope_of_artifact
from team.models import Briefing

logger = logging.getLogger(__name__)

_SCOUT_ARTIFACT_PREFIX = "scout:"
_ATLAS_ARTIFACT_PREFIX = "atlas:"
_CONTEXT_PROMOTION_THRESHOLD = 3.0
_HOTSPOT_EDIT_BOOST_CAP = 2
_HOTSPOT_EDIT_BOOST_WEIGHT = 0.25
_DOSSIER_LIST_LIMIT = 5


def stable_scout_artifact_ref(scope: str) -> str:
    """Return the canonical team artifact ref for *scope*."""
    return f"{_SCOUT_ARTIFACT_PREFIX}{scope}"


def note_work_item_context_access(
    team_run: Any,
    work_item: Any,
    metadata: Any,
    *,
    artifact: dict[str, Any] | None = None,
) -> list[str]:
    """Harvest read-path telemetry into run-scoped canonical-scope stats."""
    project_ctx = getattr(team_run, "project_context", None)
    if project_ctx is None:
        return []
    read_paths = _normalize_path_list(metadata.get("_read_paths_this_turn", []))
    if not read_paths:
        return []
    repo_root = str(getattr(project_ctx, "repo_root", "") or "")
    payload = getattr(work_item, "payload", None)
    scope_sources = _scope_sources_for_work_item(team_run, work_item, artifact=artifact)
    if not scope_sources:
        fallback_scope = _canonical_scope(read_paths)
        if fallback_scope:
            scope_sources = {fallback_scope: {"read_paths"}}

    lane_id = str(getattr(work_item, "local_id", None) or getattr(work_item, "id", "") or "").strip()
    role = str(getattr(work_item, "agent_name", "") or metadata.get("agent_name") or "").strip()
    verify_refs = _normalize_path_list(payload.get("verify") if isinstance(payload, dict) else [])
    failure_refs = _normalize_string_list(payload.get("owned_failures") if isinstance(payload, dict) else [])
    touched_scopes: list[str] = []

    for scope, source_refs in scope_sources.items():
        overlapping_reads = [
            path for path in read_paths if _scope_overlaps_file(scope, path, repo_root=repo_root)
        ]
        if not overlapping_reads:
            continue
        stats = _ensure_scope_stats(project_ctx, scope)
        if lane_id:
            stats["lane_ids"].add(lane_id)
        if role:
            stats["roles"].add(role)
        stats["source_refs"].update(source_refs)
        stats["read_paths"].update(overlapping_reads)
        stats["verify_refs"].update(verify_refs)
        stats["failure_refs"].update(failure_refs)
        stats["last_accessed_at"] = time.time()
        if role == "developer":
            stats["developer_lane_ids"].add(lane_id or role)
        elif role == "validator" and stats["developer_lane_ids"]:
            stats["validator_after_developer"] = True
        touched_scopes.append(scope)

    return sorted(set(touched_scopes))


def context_pressure_for_scope(
    team_run: Any,
    scope_paths: list[str] | tuple[str, ...] | None,
    *,
    ci_service: Any | None = None,
) -> dict[str, Any]:
    """Return same-run fan-in pressure for one canonical scope slice."""
    normalized_scope_paths = _normalize_scope_parts(scope_paths)
    project_ctx = getattr(team_run, "project_context", None)
    merged = _merge_scope_stats(project_ctx, normalized_scope_paths)
    hotspot_edit_count = _hotspot_edit_count(ci_service, normalized_scope_paths)
    lane_count = len(merged["lane_ids"])
    roles = sorted(merged["roles"])
    source_ref_count = len(merged["source_refs"])
    validator_overlap = bool(merged["validator_after_developer"])
    briefing_lane_count = len(merged["briefing_lane_ids"])
    briefing_role_count = len(merged["briefing_roles"])
    briefing_hits = int(merged["briefing_hits"])

    score = max(0, lane_count - 1)
    if {"developer", "validator"}.issubset(merged["roles"]):
        score += 1.0
    elif len(merged["roles"]) > 1:
        score += 0.5
    if source_ref_count > 1:
        score += 1.0
    if validator_overlap:
        score += 1.0
    if briefing_lane_count > 1:
        score += 0.5
    if briefing_role_count > 1:
        score += 0.25
    score += min(hotspot_edit_count, _HOTSPOT_EDIT_BOOST_CAP) * _HOTSPOT_EDIT_BOOST_WEIGHT

    reasons: list[str] = []
    if lane_count > 1:
        reasons.append(f"{lane_count} distinct lanes read this scope")
    if {"developer", "validator"}.issubset(merged["roles"]):
        reasons.append("developer and validator both depend on it")
    elif len(merged["roles"]) > 1:
        reasons.append("multiple agent roles depend on it")
    if source_ref_count > 1:
        reasons.append("multiple owned/dep scope sources converge here")
    if validator_overlap:
        reasons.append("validator revisited it after a developer touch")
    if briefing_lane_count > 1:
        reasons.append(f"inherited context rendered for {briefing_lane_count} lanes")
    if briefing_role_count > 1:
        reasons.append(f"inherited context crossed {briefing_role_count} roles")
    if briefing_hits > 0:
        reasons.append(f"inherited context rendered {briefing_hits} times")
    if hotspot_edit_count > 0:
        reasons.append(f"recent hotspot edits add a weak boost ({hotspot_edit_count})")

    return {
        "score": float(score),
        "level": _pressure_level(score),
        "distinct_lane_count": lane_count,
        "roles": roles,
        "source_ref_count": source_ref_count,
        "validator_overlap": validator_overlap,
        "briefing_lane_count": briefing_lane_count,
        "briefing_role_count": briefing_role_count,
        "briefing_hits": briefing_hits,
        "hotspot_edit_count": hotspot_edit_count,
        "promotion_threshold": _CONTEXT_PROMOTION_THRESHOLD,
        "should_promote": float(score) >= _CONTEXT_PROMOTION_THRESHOLD,
        "reasons": reasons,
    }


def record_context_promotion_signal(
    team_run: Any,
    artifact: dict[str, Any],
    pressure: dict[str, Any],
) -> bool:
    """Persist non-Atlas context-reuse signals into team memory when available."""
    try:
        from team.memory.runtime import persist_memory_record
    except Exception:
        return False
    project_ctx = getattr(team_run, "project_context", None)
    if project_ctx is None:
        return False
    scope = scope_of_artifact(artifact) or ""
    scope_paths = _artifact_scope_paths(artifact) or _normalize_scope_parts([scope])
    return persist_memory_record(
        project_key=str(getattr(project_ctx, "project_key", "") or ""),
        repo_root=str(getattr(project_ctx, "repo_root", "") or ""),
        kind="context_reuse_signal",
        scope={"paths": scope_paths},
        content={
            "canonical_scope": scope,
            "score": float(pressure.get("score") or 0.0),
            "roles": list(pressure.get("roles") or []),
            "distinct_lane_count": int(pressure.get("distinct_lane_count") or 0),
            "source_ref_count": int(pressure.get("source_ref_count") or 0),
            "validator_overlap": bool(pressure.get("validator_overlap")),
        },
        source={
            "team_run_id": str(getattr(team_run, "id", "") or ""),
            "artifact_ref": stable_scout_artifact_ref(scope) if scope else "",
            "agent": "runtime",
        },
        stale_hint="same-run context reuse signal aged out after new overlapping edits",
    )


def scout_artifact_invalidated(
    project_ctx: Any,
    artifact: dict[str, Any] | None,
) -> bool:
    """Return True when a scout artifact predates a same-run overlapping write."""
    if not isinstance(artifact, dict):
        return False
    scope = scope_of_artifact(artifact)
    if not scope:
        return False
    invalidated = getattr(project_ctx, "invalidated_scout_scopes", None)
    if not isinstance(invalidated, dict):
        return False
    invalidated_at = invalidated.get(scope)
    snapshot = _snapshot_time(artifact)
    return (
        isinstance(invalidated_at, (int, float))
        and invalidated_at > 0
        and (snapshot <= 0 or snapshot <= float(invalidated_at))
    )


def scout_artifact_reuse_status(
    team_run: Any,
    artifact: dict[str, Any] | None,
    *,
    ci_service: Any | None = None,
) -> tuple[bool, str | None]:
    """Return whether a scout artifact is safe to reuse in the current run."""
    artifact = artifact if isinstance(artifact, dict) else None
    reusable, reason = brief_reuse_status(
        artifact,
        min_scope_coverage=MIN_COMPLETE_SCOPE_COVERAGE,
    )
    if not reusable:
        return False, reason

    project_ctx = getattr(team_run, "project_context", None)
    if project_ctx is None:
        return True, None
    if scout_artifact_invalidated(project_ctx, artifact):
        return False, "same-run edits invalidated this scout brief after its snapshot"

    ledger = getattr(ci_service, "ledger", None)
    if ledger is None or artifact is None:
        return True, None

    target_paths = _artifact_scope_paths(artifact)
    scope = scope_of_artifact(artifact) or _canonical_scope(target_paths)
    if not scope:
        return True, None

    chunk = SimpleNamespace(
        subsystem=scope,
        brief=dict(artifact),
        scope_paths=list(target_paths),
        repo_root=str(getattr(project_ctx, "repo_root", "") or ""),
        snapshot_time=_snapshot_time(artifact),
        updated_at=None,
        content_hashes={},
    )
    return freshness_status(
        chunk,
        ledger=ledger,
        max_age_seconds=None,
    )


def invalidate_stale_scout_context(team_run: Any, file_path: str) -> list[str]:
    """Evict scout-backed shared context that overlaps *file_path*.

    This keeps same-run prompt injection conservative after writes: shared
    scout briefings and stable scout version metadata are removed when an
    edited path falls under their scope. Inline shared briefings and
    non-scout artifact briefings are left untouched.
    """
    project_ctx = getattr(team_run, "project_context", None)
    if project_ctx is None:
        return []
    _note_context_write(project_ctx, file_path)
    repo_root = str(getattr(project_ctx, "repo_root", "") or "")
    shared_briefings = getattr(project_ctx, "shared_briefings", None)
    shared_meta = getattr(project_ctx, "shared_briefing_meta", None)
    stable_versions = getattr(project_ctx, "stable_scout_versions", None)
    invalidated = getattr(project_ctx, "invalidated_scout_scopes", None)
    if (
        not isinstance(shared_briefings, dict)
        or not isinstance(shared_meta, dict)
        or not isinstance(stable_versions, dict)
        or not isinstance(invalidated, dict)
    ):
        return []

    stale_scopes: set[str] = set()
    for scope, briefing in list(shared_briefings.items()):
        if not _is_scout_briefing(briefing):
            continue
        if _scope_overlaps_file(scope, file_path, repo_root=repo_root):
            stale_scopes.add(scope)
    for scope in list(stable_versions.keys()):
        if _scope_overlaps_file(scope, file_path, repo_root=repo_root):
            stale_scopes.add(scope)

    if not stale_scopes:
        return []

    invalidated_at = time.time()
    for scope in sorted(stale_scopes):
        briefing = shared_briefings.get(scope)
        if _is_scout_briefing(briefing):
            shared_briefings.pop(scope, None)
            shared_meta.pop(scope, None)
        project_ctx.auto_promoted_scout_scopes.discard(scope)
        stable_versions.pop(scope, None)
        invalidated[scope] = invalidated_at
    return sorted(stale_scopes)


def store_stable_scout_artifact(
    team_run: Any,
    artifact: dict[str, Any],
    *,
    run_id: str | None = None,
) -> str | None:
    """Persist the latest scout artifact under a stable per-scope key."""
    if not isinstance(artifact, dict):
        return None
    scope = scope_of_artifact(artifact)
    if not scope:
        return None
    ref = stable_scout_artifact_ref(scope)
    existing = team_run.artifacts.load(ref)
    current_version = team_run.project_context.stable_scout_versions.get(scope)
    if current_version is None:
        current_version = _version_from_artifact(existing)
    incoming_version = _version_from_artifact(artifact, run_id=run_id)
    if isinstance(existing, dict) and not _should_replace(current_version, incoming_version):
        return ref
    team_run.artifacts.save(ref, dict(artifact))
    team_run.project_context.stable_scout_versions[scope] = incoming_version
    return ref


def auto_promote_scout_briefing(
    team_run: Any,
    artifact_ref: str,
    *,
    ci_service: Any | None = None,
    force: bool = False,
) -> bool:
    """Promote a reusable scout artifact into run-scoped shared briefings."""
    artifact = team_run.artifacts.load(artifact_ref)
    if not isinstance(artifact, dict):
        return False
    scope = scope_of_artifact(artifact)
    if not scope:
        return False
    reusable, reason = scout_artifact_reuse_status(
        team_run,
        artifact,
        ci_service=ci_service,
    )
    if not reusable:
        logger.debug(
            "scout auto-promotion skipped for %s: %s",
            scope,
            reason or "not reusable",
        )
        return False
    pressure = context_pressure_for_scope(
        team_run,
        _artifact_scope_paths(artifact) or [scope],
        ci_service=ci_service,
    )
    if not pressure["should_promote"] and not force:
        logger.debug(
            "scout auto-promotion skipped for %s: same-run pressure %.2f below threshold %.2f",
            scope,
            pressure["score"],
            pressure["promotion_threshold"],
        )
        return False

    project_ctx = team_run.project_context
    briefings = project_ctx.shared_briefings
    replaceable_scopes = team_run.project_context.auto_promoted_scout_scopes
    existing_is_replaceable = scope in replaceable_scopes
    is_new_scope = scope not in briefings
    if is_new_scope and len(briefings) >= team_run.budgets.max_shared_briefings:
        victim = evict_auto_promoted_scout_briefing(team_run)
        if victim is None:
            logger.debug(
                "scout auto-promotion skipped for %s: shared briefing cap reached",
                scope,
            )
            return False

    briefings[scope] = Briefing(
        name=stable_scout_artifact_ref(scope),
        source="artifact",
        ref=artifact_ref,
        description=_build_scope_dossier(team_run, scope, artifact, pressure, ci_service=ci_service),
    )
    stamp_shared_briefing_meta(
        project_ctx,
        scope,
        briefing=briefings[scope],
        artifact=artifact,
        provenance="auto-scout",
        stale_on_write=False,
    )
    if is_new_scope or existing_is_replaceable:
        replaceable_scopes.add(scope)
    else:
        replaceable_scopes.discard(scope)
    project_ctx.scope_promotion_counts[scope] = int(project_ctx.scope_promotion_counts.get(scope, 0)) + 1
    record_context_promotion_signal(team_run, artifact, pressure)
    return True


def evict_auto_promoted_scout_briefing(team_run: Any) -> str | None:
    victim = _select_auto_promoted_victim(team_run)
    if victim is None:
        return None
    team_run.project_context.shared_briefings.pop(victim, None)
    team_run.project_context.shared_briefing_meta.pop(victim, None)
    team_run.project_context.auto_promoted_scout_scopes.discard(victim)
    return victim


def _select_auto_promoted_victim(team_run: Any) -> str | None:
    candidates: list[tuple[float, float, float, str]] = []
    for scope in team_run.project_context.auto_promoted_scout_scopes:
        briefing = team_run.project_context.shared_briefings.get(scope)
        if briefing is None:
            continue
        if briefing.source != "artifact" or not briefing.ref:
            continue
        if not briefing.ref.startswith(_SCOUT_ARTIFACT_PREFIX):
            continue
        artifact = team_run.artifacts.load(briefing.ref)
        pressure = context_pressure_for_scope(
            team_run,
            _artifact_scope_paths(artifact) or [scope],
        )
        candidates.append(
            (
                float(pressure.get("score") or 0.0),
                _scope_coverage(artifact),
                _snapshot_time(artifact),
                scope,
            )
        )
    if not candidates:
        return None
    _, _, _, victim = min(candidates)
    return victim


def _should_replace(
    current_version: dict[str, Any] | None,
    incoming_version: dict[str, Any] | None,
) -> bool:
    current_snapshot = _version_snapshot(current_version)
    incoming_snapshot = _version_snapshot(incoming_version)
    if incoming_snapshot != current_snapshot:
        return incoming_snapshot > current_snapshot

    current_run_id = _version_run_id(current_version)
    incoming_run_id = _version_run_id(incoming_version)
    if current_run_id and incoming_run_id:
        return incoming_run_id > current_run_id
    return False


def _version_from_artifact(
    artifact: Any,
    *,
    run_id: str | None = None,
) -> dict[str, Any]:
    version: dict[str, Any] = {}
    snapshot = _snapshot_time(artifact)
    if snapshot > 0:
        version["snapshot_time"] = snapshot
    if isinstance(run_id, str) and run_id:
        version["run_id"] = run_id
    return version


def _version_snapshot(version: dict[str, Any] | None) -> float:
    if not isinstance(version, dict):
        return 0.0
    raw = version.get("snapshot_time")
    return float(raw) if isinstance(raw, (int, float)) and raw > 0 else 0.0


def _version_run_id(version: dict[str, Any] | None) -> str:
    if not isinstance(version, dict):
        return ""
    raw = version.get("run_id")
    return raw if isinstance(raw, str) else ""


def _snapshot_time(artifact: Any) -> float:
    if not isinstance(artifact, dict):
        return 0.0
    raw = artifact.get("snapshot_time")
    return float(raw) if isinstance(raw, (int, float)) and raw > 0 else 0.0


def _scope_coverage(artifact: Any) -> float:
    if not isinstance(artifact, dict):
        return -1.0
    raw = artifact.get("scope_coverage")
    return float(raw) if isinstance(raw, (int, float)) else -1.0


def _is_scout_briefing(briefing: Any) -> bool:
    return (
        isinstance(getattr(briefing, "source", None), str)
        and briefing.source == "artifact"
        and isinstance(getattr(briefing, "ref", None), str)
        and briefing.ref.startswith(_SCOUT_ARTIFACT_PREFIX)
    )


def _ensure_scope_stats(project_ctx: Any, scope: str) -> dict[str, Any]:
    stats_map = getattr(project_ctx, "scope_context_stats", None)
    if not isinstance(stats_map, dict):
        stats_map = {}
        setattr(project_ctx, "scope_context_stats", stats_map)
    stats = stats_map.get(scope)
    if not isinstance(stats, dict):
        stats = {}
        stats_map[scope] = stats
    stats.setdefault("lane_ids", set())
    stats.setdefault("roles", set())
    stats.setdefault("source_refs", set())
    stats.setdefault("read_paths", set())
    stats.setdefault("verify_refs", set())
    stats.setdefault("failure_refs", set())
    stats.setdefault("developer_lane_ids", set())
    stats.setdefault("validator_after_developer", False)
    stats.setdefault("briefing_hits", 0)
    stats.setdefault("briefing_lane_ids", set())
    stats.setdefault("briefing_roles", set())
    stats.setdefault("briefing_tiers", set())
    return stats


def _scope_sources_for_work_item(
    team_run: Any,
    work_item: Any,
    *,
    artifact: dict[str, Any] | None = None,
) -> dict[str, set[str]]:
    payload = getattr(work_item, "payload", None)
    out: dict[str, set[str]] = {}

    if isinstance(payload, dict):
        for key in ("target_paths", "touches_paths", "paths", "files", "verify", "owned_files"):
            scope = _canonical_scope(payload.get(key))
            if scope:
                out.setdefault(scope, set()).add(f"payload:{key}")
        for key in ("canonical_scope", "file_path", "path", "subsystem"):
            scope = _canonical_scope(payload.get(key))
            if scope:
                out.setdefault(scope, set()).add(f"payload:{key}")

    store = getattr(team_run, "artifacts", None)
    for dep in getattr(work_item, "dep_artifacts", []) or ():
        if store is None:
            continue
        dep_ref = getattr(dep, "artifact_ref", None)
        body = store.load(dep_ref) if dep_ref else None
        scope = scope_of_artifact(body)
        if scope:
            out.setdefault(scope, set()).add(f"dep:{dep_ref}")
    for briefing in getattr(work_item, "briefings", []) or ():
        if getattr(briefing, "source", "") != "artifact" or store is None:
            continue
        ref = getattr(briefing, "ref", None)
        body = store.load(ref) if ref else None
        scope = scope_of_artifact(body)
        if scope:
            out.setdefault(scope, set()).add(f"briefing:{ref or getattr(briefing, 'name', '')}")

    artifact_scope = scope_of_artifact(artifact)
    if artifact_scope:
        out.setdefault(artifact_scope, set()).add("artifact")

    return out


def _merge_scope_stats(project_ctx: Any, scope_paths: list[str]) -> dict[str, Any]:
    merged = {
        "lane_ids": set(),
        "roles": set(),
        "source_refs": set(),
        "read_paths": set(),
        "verify_refs": set(),
        "failure_refs": set(),
        "developer_lane_ids": set(),
        "validator_after_developer": False,
        "briefing_hits": 0,
        "briefing_lane_ids": set(),
        "briefing_roles": set(),
        "briefing_tiers": set(),
    }
    stats_map = getattr(project_ctx, "scope_context_stats", None)
    if not isinstance(stats_map, dict):
        return merged
    repo_root = str(getattr(project_ctx, "repo_root", "") or "")
    for scope, stats in stats_map.items():
        if scope_paths and not any(_scope_overlaps_file(scope, path, repo_root=repo_root) for path in scope_paths):
            continue
        for key in (
            "lane_ids",
            "roles",
            "source_refs",
            "read_paths",
            "verify_refs",
            "failure_refs",
            "developer_lane_ids",
            "briefing_lane_ids",
            "briefing_roles",
            "briefing_tiers",
        ):
            merged[key].update(_coerce_str_set(stats.get(key)))
        merged["validator_after_developer"] = bool(
            merged["validator_after_developer"] or stats.get("validator_after_developer")
        )
        merged["briefing_hits"] += int(stats.get("briefing_hits") or 0)
    return merged


def _build_scope_dossier(
    team_run: Any,
    scope: str,
    artifact: dict[str, Any],
    pressure: dict[str, Any],
    *,
    ci_service: Any | None = None,
) -> str:
    project_ctx = getattr(team_run, "project_context", None)
    merged = _merge_scope_stats(project_ctx, _artifact_scope_paths(artifact) or [scope])
    key_symbols, neighborhood = _symbol_dossier(ci_service, artifact)
    verify_surface = _summarize_values(merged["verify_refs"])
    failure_surface = _summarize_values(merged["failure_refs"])
    source_refs = _summarize_values(list(merged["source_refs"]) + [stable_scout_artifact_ref(scope)])
    version = getattr(project_ctx, "stable_scout_versions", {}).get(scope, {})
    freshness_parts = [f"snapshot_time={_snapshot_time(artifact):.3f}"]
    run_id = _version_run_id(version)
    if run_id:
        freshness_parts.append(f"run_id={run_id}")
    return "\n".join(
        [
            "Scope dossier",
            f"- owner cluster: {scope}",
            f"- context_hotspot_score: {float(pressure.get('score') or 0.0):.2f} ({pressure.get('level') or 'low'})",
            f"- key symbols: {key_symbols or 'none'}",
            f"- one-hop symbol neighborhood: {neighborhood or 'none'}",
            f"- verification surface: verify={verify_surface}; failures={failure_surface}",
            f"- inherited reuse: briefing_hits={int(merged['briefing_hits'])}; briefing_lanes={_summarize_values(merged['briefing_lane_ids'])}",
            f"- freshness: {', '.join(freshness_parts)}",
            f"- source artifact refs: {source_refs}",
        ]
    )


def stamp_shared_briefing_meta(
    project_ctx: Any,
    scope: str,
    *,
    briefing: Any,
    artifact: dict[str, Any] | None,
    provenance: str | None = None,
    stale_on_write: bool | None = None,
    source_coherence_token: str | None = None,
    source_packet_freshness: str | None = None,
) -> dict[str, Any]:
    """Attach a small context envelope to one active shared briefing."""
    meta = _ensure_shared_briefing_meta(project_ctx, scope)
    kind = _briefing_kind(briefing=briefing, artifact=artifact)
    now = time.time()
    meta["kind"] = kind
    meta["provenance"] = provenance or _briefing_provenance(briefing=briefing, artifact=artifact)
    meta["scope_paths"] = _shared_scope_paths(scope, artifact)
    meta["stale_on_write"] = bool(stale_on_write if stale_on_write is not None else kind != "structural")
    meta["created_at"] = float(meta.get("created_at") or now)
    meta["updated_at"] = now
    meta["repo_epoch"] = int(getattr(project_ctx, "repo_epoch", 0) or 0)
    meta["scope_write_epoch"] = _shared_scope_write_epoch(project_ctx, scope)
    if source_coherence_token:
        meta["source_coherence_token"] = source_coherence_token
    if source_packet_freshness:
        meta["source_packet_freshness"] = source_packet_freshness
    return meta


def note_inherited_context_render(
    project_ctx: Any,
    scope: str,
    work_item: Any,
    *,
    tier: str,
) -> None:
    """Record that inherited context for *scope* was rendered into a prompt."""
    if project_ctx is None or not scope:
        return
    lane_id = str(getattr(work_item, "local_id", None) or getattr(work_item, "id", "") or "").strip()
    role = str(getattr(work_item, "agent_name", "") or "").strip()
    if tier == "shared":
        meta = _ensure_shared_briefing_meta(project_ctx, scope)
        meta["render_count"] = int(meta.get("render_count") or 0) + 1
        meta["last_rendered_at"] = time.time()
        if lane_id:
            meta["consumer_lane_ids"].add(lane_id)
        if role:
            meta["consumer_roles"].add(role)
    stats = _ensure_scope_stats(project_ctx, scope)
    stats["briefing_hits"] = int(stats.get("briefing_hits") or 0) + 1
    if lane_id:
        stats["briefing_lane_ids"].add(lane_id)
    if role:
        stats["briefing_roles"].add(role)
    if tier:
        stats["briefing_tiers"].add(tier)


def inherited_context_line(
    project_ctx: Any,
    scope: str | None,
    *,
    briefing: Any = None,
    artifact: dict[str, Any] | None = None,
    tier: str,
) -> str:
    """Return a compact prompt hint about inherited context freshness."""
    scope = str(scope or "").strip()
    if project_ctx is None:
        if not scope:
            return f"context: tier={tier}; freshness=fresh"
        return f"context: tier={tier}; freshness=fresh; scope={scope}"
    if not scope:
        return f"context: tier={tier}; freshness=fresh"
    if tier != "shared":
        freshness = "stale" if scout_artifact_invalidated(project_ctx, artifact) else "fresh"
        return f"context: tier={tier}; freshness={freshness}; scope={scope}"
    meta = _ensure_shared_briefing_meta(project_ctx, scope)
    kind = str(meta.get("kind") or _briefing_kind(briefing=briefing, artifact=artifact))
    provenance = str(meta.get("provenance") or _briefing_provenance(briefing=briefing, artifact=artifact))
    freshness = _shared_context_freshness(project_ctx, scope, artifact=artifact)
    consumer_count = len(_coerce_str_set(meta.get("consumer_lane_ids")))
    return (
        f"context: tier={tier}; kind={kind}; provenance={provenance}; "
        f"freshness={freshness}; consumers={consumer_count}"
    )


def _symbol_dossier(ci_service: Any | None, artifact: dict[str, Any]) -> tuple[str, str]:
    symbol_index = getattr(ci_service, "symbol_index", None)
    if symbol_index is None:
        return "", ""
    symbol_names: list[str] = []
    for path in _artifact_scope_paths(artifact):
        try:
            symbols = symbol_index.file_symbols(path)
        except Exception:
            continue
        for sym in symbols:
            name = getattr(sym, "name", None)
            if isinstance(name, str) and name.strip():
                symbol_names.append(name.strip())
    key_symbols = _summarize_values(symbol_names[:_DOSSIER_LIST_LIMIT])
    neighborhood = _summarize_values(
        _normalize_string_list(artifact.get("entry_points")) or symbol_names[_DOSSIER_LIST_LIMIT:]
    )
    return key_symbols, neighborhood


def _hotspot_edit_count(ci_service: Any | None, scope_paths: list[str]) -> int:
    arbiter = getattr(ci_service, "arbiter", None)
    if arbiter is None:
        return 0
    try:
        hotspots = arbiter.hotspots(limit=25)
    except Exception:
        return 0
    max_edits = 0
    for file_path, count in hotspots:
        if scope_paths and not any(_paths_overlap(str(file_path), scope) for scope in scope_paths):
            continue
        max_edits = max(max_edits, int(count))
    return max_edits


def _pressure_level(score: float) -> str:
    if score >= _CONTEXT_PROMOTION_THRESHOLD:
        return "high"
    if score > 0:
        return "medium"
    return "low"


def _artifact_scope_paths(artifact: dict[str, Any] | None) -> list[str]:
    if not isinstance(artifact, dict):
        return []
    return _normalize_scope_parts(artifact.get("target_paths"))


def _note_context_write(project_ctx: Any, file_path: str) -> None:
    if project_ctx is None:
        return
    repo_root = str(getattr(project_ctx, "repo_root", "") or "")
    shared_briefings = getattr(project_ctx, "shared_briefings", {}) or {}
    shared_meta = getattr(project_ctx, "shared_briefing_meta", {}) or {}
    stable_versions = getattr(project_ctx, "stable_scout_versions", {}) or {}
    epochs = getattr(project_ctx, "scope_write_epochs", None)
    if not isinstance(epochs, dict):
        epochs = {}
        setattr(project_ctx, "scope_write_epochs", epochs)
    setattr(project_ctx, "repo_epoch", int(getattr(project_ctx, "repo_epoch", 0) or 0) + 1)
    scope_keys = set(shared_briefings.keys()) | set(shared_meta.keys()) | set(stable_versions.keys())
    for scope in scope_keys:
        if _scope_overlaps_file(scope, file_path, repo_root=repo_root):
            epochs[scope] = int(epochs.get(scope) or 0) + 1


def _canonical_scope(raw: Any) -> str:
    if isinstance(raw, str):
        return canonicalize_scope([part for part in raw.split("|") if isinstance(part, str)])
    if isinstance(raw, (list, tuple)):
        return canonicalize_scope([item for item in raw if isinstance(item, str)])
    return ""


def _ensure_shared_briefing_meta(project_ctx: Any, scope: str) -> dict[str, Any]:
    meta_map = getattr(project_ctx, "shared_briefing_meta", None)
    if not isinstance(meta_map, dict):
        meta_map = {}
        setattr(project_ctx, "shared_briefing_meta", meta_map)
    meta = meta_map.get(scope)
    if not isinstance(meta, dict):
        meta = {}
        meta_map[scope] = meta
    meta.setdefault("consumer_lane_ids", set())
    meta.setdefault("consumer_roles", set())
    meta.setdefault("render_count", 0)
    meta.setdefault("scope_paths", [])
    return meta


def _briefing_kind(*, briefing: Any, artifact: dict[str, Any] | None) -> str:
    if isinstance(getattr(briefing, "source", None), str) and briefing.source == "inline":
        return "runtime"
    ref = str(getattr(briefing, "ref", "") or "")
    if ref.startswith(_SCOUT_ARTIFACT_PREFIX) or ref.startswith(_ATLAS_ARTIFACT_PREFIX):
        return "structural"
    if artifact and (scope_of_artifact(artifact) or _artifact_scope_paths(artifact)):
        return "structural"
    return "runtime"


def _briefing_provenance(*, briefing: Any, artifact: dict[str, Any] | None) -> str:
    ref = str(getattr(briefing, "ref", "") or "")
    if ref.startswith(_ATLAS_ARTIFACT_PREFIX):
        return "atlas"
    if ref.startswith(_SCOUT_ARTIFACT_PREFIX):
        return "scout"
    if isinstance(getattr(briefing, "source", None), str) and briefing.source == "inline":
        return "manual-inline"
    if artifact and (scope_of_artifact(artifact) or _artifact_scope_paths(artifact)):
        return "manual-artifact"
    return "inline"


def _shared_scope_paths(scope: str, artifact: dict[str, Any] | None) -> list[str]:
    scope_paths = _artifact_scope_paths(artifact)
    if scope_paths:
        return scope_paths
    return _normalize_scope_parts([scope])


def _shared_scope_write_epoch(project_ctx: Any, scope: str) -> int:
    epochs = getattr(project_ctx, "scope_write_epochs", None)
    if not isinstance(epochs, dict):
        return 0
    return int(epochs.get(scope) or 0)


def _shared_context_freshness(
    project_ctx: Any,
    scope: str,
    *,
    artifact: dict[str, Any] | None,
) -> str:
    if scout_artifact_invalidated(project_ctx, artifact):
        return "stale"
    meta_map = getattr(project_ctx, "shared_briefing_meta", None)
    meta = meta_map.get(scope) if isinstance(meta_map, dict) else None
    if not isinstance(meta, dict):
        return "fresh"
    if not bool(meta.get("stale_on_write")):
        return "fresh"
    current_scope_epoch = _shared_scope_write_epoch(project_ctx, scope)
    if current_scope_epoch > int(meta.get("scope_write_epoch") or 0):
        return "caution"
    current_repo_epoch = int(getattr(project_ctx, "repo_epoch", 0) or 0)
    if not meta.get("scope_paths") and current_repo_epoch > int(meta.get("repo_epoch") or 0):
        return "caution"
    return "fresh"


def shared_context_summary_for_scope(
    team_run: Any | None,
    scope_paths: list[str] | tuple[str, ...] | None,
) -> list[dict[str, Any]]:
    """Return compact shared-context summaries for scope-packet coordination."""
    if team_run is None:
        return []
    project_ctx = getattr(team_run, "project_context", None)
    if project_ctx is None:
        return []
    shared_briefings = getattr(project_ctx, "shared_briefings", None)
    if not isinstance(shared_briefings, dict):
        return []
    normalized = _normalize_scope_parts(scope_paths)
    repo_root = str(getattr(project_ctx, "repo_root", "") or "")
    out: list[dict[str, Any]] = []
    for scope, briefing in shared_briefings.items():
        if normalized and not any(
            _scope_overlaps_file(scope, path, repo_root=repo_root) for path in normalized
        ):
            continue
        artifact = None
        ref = getattr(briefing, "ref", None)
        store = getattr(team_run, "artifacts", None)
        if getattr(briefing, "source", "") == "artifact" and ref and store is not None:
            loaded = store.load(ref)
            artifact = loaded if isinstance(loaded, dict) else None
        meta = _ensure_shared_briefing_meta(project_ctx, scope)
        out.append(
            {
                "scope": scope,
                "kind": str(meta.get("kind") or _briefing_kind(briefing=briefing, artifact=artifact)),
                "provenance": str(
                    meta.get("provenance") or _briefing_provenance(briefing=briefing, artifact=artifact)
                ),
                "freshness": _shared_context_freshness(project_ctx, scope, artifact=artifact),
                "consumer_count": len(_coerce_str_set(meta.get("consumer_lane_ids"))),
                "render_count": int(meta.get("render_count") or 0),
                "scope_write_epoch": _shared_scope_write_epoch(project_ctx, scope),
            }
        )
    out.sort(key=lambda item: item["scope"])
    return out


def _normalize_scope_parts(raw: Any) -> list[str]:
    scope = _canonical_scope(raw)
    return [part for part in scope.split("|") if part]


def _normalize_path_list(raw: Any) -> list[str]:
    out: list[str] = []
    for item in raw if isinstance(raw, list) else [raw] if isinstance(raw, str) else []:
        if isinstance(item, str):
            cleaned = item.strip()
            if cleaned:
                out.append(cleaned)
    return out


def _normalize_string_list(raw: Any) -> list[str]:
    out: list[str] = []
    if not isinstance(raw, list):
        return out
    seen: set[str] = set()
    for item in raw:
        if not isinstance(item, str):
            continue
        cleaned = item.strip()
        if not cleaned or cleaned in seen:
            continue
        seen.add(cleaned)
        out.append(cleaned)
    return out


def _coerce_str_set(raw: Any) -> set[str]:
    if isinstance(raw, set):
        return {item for item in raw if isinstance(item, str) and item}
    if isinstance(raw, list):
        return {item for item in raw if isinstance(item, str) and item}
    return set()


def _summarize_values(raw: Any) -> str:
    values = sorted(_coerce_str_set(raw) if not isinstance(raw, list) else set(_normalize_string_list(raw)))
    if not values:
        return "none"
    limited = values[:_DOSSIER_LIST_LIMIT]
    suffix = "…" if len(values) > _DOSSIER_LIST_LIMIT else ""
    return ", ".join(limited) + suffix


def _scope_overlaps_file(scope: str, file_path: str, *, repo_root: str) -> bool:
    scope_parts = [part for part in str(scope or "").split("|") if part.strip()]
    if not scope_parts:
        return False
    file_variants = _path_variants(file_path, repo_root=repo_root)
    for part in scope_parts:
        scope_variants = _path_variants(part, repo_root=repo_root)
        for candidate in file_variants:
            for target in scope_variants:
                if _paths_overlap(candidate, target):
                    return True
    return False


def _path_variants(path: str, *, repo_root: str) -> set[str]:
    cleaned = _normalise_path(path)
    if not cleaned:
        return set()
    out = {cleaned}
    root = _normalise_path(repo_root)
    if not root:
        return out
    if cleaned.startswith(root + "/"):
        out.add(cleaned[len(root) + 1 :])
    elif not cleaned.startswith("/"):
        out.add(f"{root}/{cleaned}")
    return out


def _normalise_path(path: str) -> str:
    return str(path or "").strip().replace("\\", "/").removeprefix("./").rstrip("/")


def _paths_overlap(path_a: str, path_b: str) -> bool:
    left = _normalise_path(path_a)
    right = _normalise_path(path_b)
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

"""Inspect run-scoped inherited context with live scope freshness."""

from __future__ import annotations

import json
from typing import Any

from team.context.scout_briefings import shared_context_summary_for_scope
from team.runtime.registry import get as _get_team_run
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from tools.daytona_toolkit.ci_integration import refresh_scope_baseline
from tools.daytona_toolkit.coordination import normalize_scope_paths

_PREVIEW_CHAR_CAP = 240


def _resolve_scope_paths(
    scope_paths: list[str] | None,
    context: ToolExecutionContext,
) -> list[str]:
    requested = normalize_scope_paths(scope_paths or [])
    if requested:
        return requested
    baseline = context.metadata.get("scope_packet")
    if isinstance(baseline, dict):
        baseline_paths = baseline.get("scope_paths")
        if isinstance(baseline_paths, list):
            return normalize_scope_paths([str(item) for item in baseline_paths if isinstance(item, str)])
    return normalize_scope_paths(context.metadata.get("default_scope_paths") or [])


def _preview_text(value: Any) -> str:
    if isinstance(value, dict):
        rendered = json.dumps(value, sort_keys=True, default=str)
    else:
        rendered = str(value or "")
    rendered = " ".join(rendered.split())
    if len(rendered) <= _PREVIEW_CHAR_CAP:
        return rendered
    return rendered[: _PREVIEW_CHAR_CAP - 1] + "…"


def _body_preview(
    *,
    team_run: Any,
    briefing: Any,
    include_body: bool,
) -> str | None:
    if not include_body:
        return None
    if str(getattr(briefing, "source", "") or "") == "inline":
        return _preview_text(getattr(briefing, "inline", "") or "")
    ref = getattr(briefing, "ref", None)
    store = getattr(team_run, "artifacts", None)
    if not ref or store is None:
        return None
    loaded = store.load(ref)
    if loaded is None:
        return "[missing artifact]"
    return _preview_text(loaded)


@tool(
    name="inspect_inherited_context",
    description=(
        "Inspect run-scoped inherited context for the current scope, including "
        "freshness, provenance, and the live coherence token."
    ),
    read_only=True,
)
async def inspect_inherited_context(
    scope_paths: list[str] | None = None,
    include_body: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Inspect same-run inherited context and refresh the scoped baseline.

    Args:
        scope_paths: Optional scope paths to inspect. Defaults to the current live scope packet or work-item scope.
        include_body: Include a short preview of each matching briefing body when true.

    Returns:
        scope_paths (list): Scope paths used for the lookup
        coherence_token (string): Current live coherence token for the scoped slice
        freshness (string): Current freshness of the scoped packet
        shared_context (list): Matching inherited-context entries with provenance and freshness
    """
    team_run_id = str(context.metadata.get("team_run_id") or "").strip()
    if not team_run_id:
        return ToolResult(
            output="inspect_inherited_context unavailable: no team_run_id in execution context",
            is_error=True,
        )
    team_run = _get_team_run(team_run_id)
    if team_run is None:
        return ToolResult(
            output=f"inspect_inherited_context: team_run {team_run_id!r} not registered",
            is_error=True,
        )

    requested = _resolve_scope_paths(scope_paths, context)
    previous_token = str(context.metadata.get("coherence_token") or "")
    packet = refresh_scope_baseline(context, scope_paths=requested or None)
    resolved_scope_paths = normalize_scope_paths(packet.get("scope_paths") or requested)
    current_token = str(packet.get("coherence_token") or "")
    summaries = shared_context_summary_for_scope(team_run, resolved_scope_paths)
    shared_briefings = getattr(team_run.project_context, "shared_briefings", {}) or {}
    shared_meta = getattr(team_run.project_context, "shared_briefing_meta", {}) or {}

    entries: list[dict[str, Any]] = []
    for summary in summaries:
        scope = str(summary.get("scope") or "")
        briefing = shared_briefings.get(scope)
        if briefing is None:
            continue
        meta = shared_meta.get(scope) if isinstance(shared_meta, dict) else None
        entry = dict(summary)
        entry["name"] = str(getattr(briefing, "name", "") or "")
        entry["source"] = str(getattr(briefing, "source", "") or "")
        entry["description"] = str(getattr(briefing, "description", "") or "")
        if getattr(briefing, "ref", None):
            entry["ref"] = str(getattr(briefing, "ref", "") or "")
        if isinstance(meta, dict) and meta.get("source_coherence_token"):
            entry["source_coherence_token"] = str(meta.get("source_coherence_token") or "")
        if isinstance(meta, dict) and meta.get("source_packet_freshness"):
            entry["source_packet_freshness"] = str(meta.get("source_packet_freshness") or "")
        preview = _body_preview(team_run=team_run, briefing=briefing, include_body=include_body)
        if preview is not None:
            entry["body_preview"] = preview
        entries.append(entry)

    payload = {
        "scope_paths": resolved_scope_paths,
        "baseline_coherence_token": previous_token,
        "coherence_token": current_token,
        "coherence_state": "fresh" if not previous_token or previous_token == current_token else "drifted",
        "freshness": str(packet.get("freshness") or "fresh"),
        "admission": packet.get("admission") if isinstance(packet.get("admission"), dict) else {},
        "shared_context": entries,
    }
    return ToolResult(
        output=json.dumps(payload, indent=2, default=str),
        metadata={
            "scope_packet": packet,
            "coherence_token": current_token,
        },
    )

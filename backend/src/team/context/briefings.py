"""Render briefings into a prompt preamble for executors.

Three tiers of context flow into an executor's prompt, in descending
priority for dedup (higher tier wins):

1. ``project_context.shared_briefings`` — run-scoped, keyed by
   canonical_scope. (Phase 1 §13.)
2. ``wi.dep_artifacts`` — DAG-snapshotted at PENDING→READY by the
   Dispatcher from each satisfied dependency subtree. (Phase 1 §2.)
3. ``wi.briefings`` — explicit briefings the planner attached to the
   child's spec. (Phase 1 §1.)

Dedup key is ``canonical_scope`` when available (loaded from the brief
body), falling back to ``artifact_ref`` when the brief has no scope
annotation (e.g. inline briefings or non-scout artifacts).
"""

from __future__ import annotations

from typing import Any, TYPE_CHECKING

from team.context.canonicalize import scope_of_artifact
from team.context.scout_briefings import (
    inherited_context_line,
    note_inherited_context_render,
    scout_artifact_invalidated,
)
from team.models import Briefing, BudgetConfig, WorkItem

if TYPE_CHECKING:
    from team.artifacts.store import InMemoryArtifactStore
    from team.context.project import ProjectContext


_HEADER = "## Briefing from parent"
_SHARED_HEADER = "## Shared context"
_DEPS_HEADER = "## From deps"
_EXPLICIT_HEADER = "## From parent"


def _truncate(body: Any, max_bytes: int) -> str:
    text = body if isinstance(body, str) else repr(body)
    data = text.encode("utf-8")
    if len(data) <= max_bytes:
        return text
    return data[:max_bytes].decode("utf-8", errors="ignore") + "\n…[truncated]"




def _dedupe_name(name: str, used: set[str]) -> str:
    if name not in used:
        used.add(name)
        return name
    i = 2
    while f"{name}_{i}" in used:
        i += 1
    unique = f"{name}_{i}"
    used.add(unique)
    return unique


def _format_section(header: str, title: str, description: str | None, body: str) -> str:
    if description:
        return f"{header} — {title}:\n{description}\n{body}"
    return f"{header} — {title}:\n{body}"


def _compose_description(context_line: str, description: str | None) -> str:
    if description:
        return f"{context_line}\n{description}"
    return context_line


def render_briefings(
    wi: WorkItem,
    artifact_store: "InMemoryArtifactStore",
    project_context: "ProjectContext | None" = None,
    budgets: BudgetConfig | None = None,
) -> str:
    """Render inherited context with tiered dedupe plus freshness hints."""
    max_bytes = (budgets or BudgetConfig()).max_briefing_bytes
    sections: list[str] = []
    seen_scopes: set[str] = set()
    seen_refs: set[str] = set()
    used_names: set[str] = set()

    def _claim(scope: str | None, ref: str | None) -> bool:
        """Return True if this entry is novel and should be rendered."""
        if scope is not None:
            if scope in seen_scopes:
                return False
            seen_scopes.add(scope)
            return True
        if ref is not None:
            if ref in seen_refs:
                return False
            seen_refs.add(ref)
            return True
        return True

    # Tier 1 — shared_briefings (highest priority). Keyed by canonical_scope.
    # Snapshot into a list so concurrent writes (e.g. a sibling calling
    # ``share_briefing``) cannot mutate the dict mid-iteration if this
    # function ever acquires an ``await`` point in the future.
    shared_src = project_context.shared_briefings if project_context is not None else {}
    shared_items = list(shared_src.items())
    for key, b in shared_items:
        body = _load_brief(b, artifact_store)
        if scout_artifact_invalidated(project_context, body):
            continue
        scope = key or scope_of_artifact(body)
        if not _claim(scope, b.ref):
            continue
        note_inherited_context_render(project_context, scope or "", wi, tier="shared")
        name = _dedupe_name(b.name, used_names)
        title = f"{name} [{scope}]" if scope else name
        description = _compose_description(
            inherited_context_line(project_context, scope, briefing=b, artifact=body, tier="shared"),
            b.description,
        )
        sections.append(
            _format_section(
                _SHARED_HEADER, title, description, _truncate(body, max_bytes)
            )
        )

    # Tier 2 — dep_artifacts
    for dep in wi.dep_artifacts:
        body = artifact_store.load(dep.artifact_ref)
        if scout_artifact_invalidated(project_context, body):
            continue
        scope = scope_of_artifact(body)
        if not _claim(scope, dep.artifact_ref):
            continue
        note_inherited_context_render(project_context, scope or "", wi, tier="deps")
        raw_name = dep.display_name or dep.source_wi_id
        name = _dedupe_name(raw_name, used_names)
        title = f"{name} [{scope}]" if scope else name
        description = inherited_context_line(project_context, scope, artifact=body, tier="deps")
        sections.append(
            _format_section(_DEPS_HEADER, title, description, _truncate(body, max_bytes))
        )

    # Tier 3 — explicit briefings
    for b in wi.briefings:
        body = _load_brief(b, artifact_store)
        if scout_artifact_invalidated(project_context, body):
            continue
        scope = scope_of_artifact(body)
        ref = b.ref if b.source == "artifact" else None
        if not _claim(scope, ref):
            continue
        note_inherited_context_render(project_context, scope or "", wi, tier="explicit")
        name = _dedupe_name(b.name, used_names)
        description = _compose_description(
            inherited_context_line(project_context, scope, briefing=b, artifact=body, tier="explicit"),
            b.description,
        )
        sections.append(
            _format_section(
                _EXPLICIT_HEADER, name, description, _truncate(body, max_bytes)
            )
        )

    if not sections:
        return ""
    return f"{_HEADER}\n\n" + "\n\n".join(sections)


def _load_brief(b: Briefing, artifact_store: "InMemoryArtifactStore") -> Any:
    if b.source == "inline":
        return b.inline or ""
    assert b.ref is not None
    body = artifact_store.load(b.ref)
    if body is None:
        return f"[missing artifact {b.ref}]"
    return body

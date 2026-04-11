"""Tier 1 — project-level context for a TeamRun."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from team.models import Briefing


@dataclass
class ProjectContext:
    goal: str = ""
    user_request: str = ""
    rationale_history: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    # Run-scoped shared briefings (§13). Keyed by canonical_scope; written
    # explicitly via the ``share_briefing`` tool, read automatically by
    # ``render_briefings`` for every executor and spawned subagent.
    shared_briefings: dict[str, Briefing] = field(default_factory=dict)
    # Lightweight metadata for active shared briefings. Keyed by the same
    # canonical scope as ``shared_briefings`` so inherited context can carry
    # provenance, freshness, and reuse hints without changing the briefing
    # transport shape seen by workers.
    shared_briefing_meta: dict[str, dict[str, Any]] = field(default_factory=dict)
    # Runtime-owned scout scopes that may be displaced under briefing
    # pressure. Explicit promotions remove a scope from this set.
    auto_promoted_scout_scopes: set[str] = field(default_factory=set)
    # Stable scout replacement metadata keyed by canonical scope. Kept in
    # run memory so equal/missing snapshot ties do not degrade to
    # last-writer-wins.
    stable_scout_versions: dict[str, dict[str, Any]] = field(default_factory=dict)
    # Run-scoped fan-in telemetry keyed by canonical scope. Values are small
    # mutable dicts containing sets/counters used to decide whether a stable
    # scout artifact is worth promoting into shared same-run context.
    scope_context_stats: dict[str, dict[str, Any]] = field(default_factory=dict)
    # Successful same-run auto-promotions keyed by canonical scope. Used to
    # avoid persisting every reusable scout into Atlas.
    scope_promotion_counts: dict[str, int] = field(default_factory=dict)
    # Timestamped invalidation markers for scout-backed context keyed by
    # canonical scope. Used to suppress stale scout artifacts that were read
    # before an overlapping write landed in the same run.
    invalidated_scout_scopes: dict[str, float] = field(default_factory=dict)
    # Monotonic same-run edit generations. ``repo_epoch`` increments on every
    # overlapping write the runtime observes; ``scope_write_epochs`` keeps a
    # narrower per-scope counter for inherited/shared context freshness.
    repo_epoch: int = 0
    scope_write_epochs: dict[str, int] = field(default_factory=dict)
    # Phase 2 — project identity for the persistent atlas. Both fields
    # default to empty strings; atlas tools treat an empty ``project_key``
    # as "atlas disabled" and degrade gracefully.
    project_key: str = ""
    repo_root: str = ""

    def add_rationale(self, text: str) -> None:
        if text:
            self.rationale_history.append(text)

    def add_note(self, text: str) -> None:
        if text:
            self.notes.append(text)

    def to_dict(self) -> dict[str, Any]:
        return {
            "goal": self.goal,
            "user_request": self.user_request,
            "rationale_history": list(self.rationale_history),
            "notes": list(self.notes),
            "project_key": self.project_key,
            "repo_root": self.repo_root,
            "shared_briefings": {
                scope: {
                    "name": b.name,
                    "source": b.source,
                    "ref": b.ref,
                    "inline": b.inline,
                    "description": b.description,
                }
                for scope, b in self.shared_briefings.items()
            },
            "shared_briefing_meta": {
                scope: {
                    key: sorted(value) if isinstance(value, set) else value
                    for key, value in meta.items()
                }
                for scope, meta in self.shared_briefing_meta.items()
            },
            "auto_promoted_scout_scopes": sorted(self.auto_promoted_scout_scopes),
            "stable_scout_versions": {
                scope: dict(version)
                for scope, version in self.stable_scout_versions.items()
            },
            "scope_context_stats": {
                scope: {
                    key: sorted(value) if isinstance(value, set) else value
                    for key, value in stats.items()
                }
                for scope, stats in self.scope_context_stats.items()
            },
            "scope_promotion_counts": {
                scope: int(count)
                for scope, count in self.scope_promotion_counts.items()
            },
            "invalidated_scout_scopes": {
                scope: float(ts)
                for scope, ts in self.invalidated_scout_scopes.items()
            },
            "repo_epoch": int(self.repo_epoch),
            "scope_write_epochs": {
                scope: int(epoch)
                for scope, epoch in self.scope_write_epochs.items()
            },
        }

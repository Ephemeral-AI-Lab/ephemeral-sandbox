"""``submit_atlas`` — posthook submit tool for ``atlas_builder`` / ``atlas_refresher``.

The tool is the single commit point for Project Atlas writes:

1. Validates each chunk (``subsystem`` derived from the brief when omitted).
2. Resolves the owning ``TeamRun`` via the in-process registry to read
   ``project_key`` and ``repo_root`` off its :class:`ProjectContext`.
3. Snapshots per-file content hashes for every path under the chunk's
   scope via :func:`team.atlas.freshness.hash_paths_under` so cold-start
   freshness checks work without git.
4. Calls :meth:`AtlasStore.upsert_chunks` — a single SQLAlchemy
   transaction that upserts the header + N chunks atomically.
5. Stashes a :class:`SubmittedSummary` into the posthook metadata slot so
   the Executor's existing completion path handles the work item without
   a new submission type.

No Executor changes — the atlas integration is entirely contained within
this module and :mod:`team.atlas`.
"""

from __future__ import annotations

import logging
import time
from typing import Any

from pydantic import BaseModel, Field, field_validator

from team.atlas.freshness import hash_paths_under
from team.atlas.store import AtlasChunk, AtlasStore, get_default_store
from team.context.canonicalize import scope_of_artifact
from team.runtime.registry import get as _get_team_run
from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool, _decode_json_array_string
from tools.posthook.submit_summary import SubmittedSummary

logger = logging.getLogger(__name__)


class _SubmitAtlasChunk(BaseModel):
    """One chunk in a ``submit_atlas`` payload.

    ``subsystem`` is optional: when omitted the posthook derives it from
    the brief's ``canonical_scope`` (or its ``target_paths``) so the
    writer only has to supply scout-shaped briefs and not a second key.
    """

    subsystem: str | None = None
    brief: dict[str, Any] = Field(
        ...,
        description=(
            "Scout-shaped brief body (same schema as Phase 1 briefs). "
            "Must include ``target_paths`` so a canonical scope can be derived."
        ),
    )


class SubmitAtlasInput(BaseModel):
    chunks: list[_SubmitAtlasChunk] = Field(
        ...,
        description=(
            "One or more atlas chunks to write. Each chunk's ``subsystem`` "
            "defaults to the brief's canonical scope when not set explicitly."
        ),
        min_length=1,
    )
    rationale: str | None = Field(
        default=None,
        description="Optional short note explaining why the atlas was updated.",
    )

    @field_validator("chunks", mode="before")
    @classmethod
    def _deserialize_chunks(cls, value: Any) -> Any:
        return _decode_json_array_string(value)


class SubmitAtlasTool(SubmitPosthookTool):
    name: str = "submit_atlas"
    description: str = (
        "Commit one or more scout briefs to the persistent Project Atlas. "
        "Each chunk is upserted transactionally under (project_key, "
        "subsystem). Must be called exactly once at the end of an atlas "
        "build/refresh run."
    )
    input_model = SubmitAtlasInput
    default_metadata_key: str = "submitted_atlas"

    def _build_payload(
        self, arguments: BaseModel, context: ToolExecutionContext
    ) -> tuple[Any, str | None]:
        assert isinstance(arguments, SubmitAtlasInput)

        team_run_id = context.metadata.get("team_run_id")
        if not team_run_id:
            return None, "submit_atlas requires a team_run_id in the execution context"
        team_run = _get_team_run(team_run_id)
        if team_run is None:
            return None, f"submit_atlas: team_run {team_run_id!r} not registered"

        project_ctx = team_run.project_context
        project_key = getattr(project_ctx, "project_key", "") or ""
        repo_root = getattr(project_ctx, "repo_root", "") or ""
        if not project_key or not repo_root:
            return None, (
                "submit_atlas: TeamRun has no project_key/repo_root; "
                "atlas is disabled for this run (supply repo_root at TeamRun "
                "construction to enable)."
            )

        chunks: list[AtlasChunk] = []
        seen: set[str] = set()
        for idx, raw in enumerate(arguments.chunks):
            subsystem, err = _resolve_subsystem(raw, idx)
            if err is not None:
                return None, err
            if subsystem in seen:
                return None, f"submit_atlas: duplicate subsystem {subsystem!r} at index {idx}"
            seen.add(subsystem)
            # Capture snapshot_time BEFORE reading files — this is the
            # ledger cutoff used by freshness checks. A brief-supplied
            # value (set by the scout at read-time) is strictly better
            # because it was taken earlier; we only fall back to "now"
            # when the scout didn't record one.
            brief_snapshot = raw.brief.get("snapshot_time") if isinstance(raw.brief, dict) else None
            snapshot_time = (
                float(brief_snapshot)
                if isinstance(brief_snapshot, (int, float)) and brief_snapshot > 0
                else time.time()
            )
            target_paths = _target_paths(raw.brief)
            content_hashes = hash_paths_under(target_paths, repo_root)
            symbol_ids = _collect_symbol_ids(context, content_hashes.keys())
            chunks.append(
                AtlasChunk(
                    subsystem=subsystem,
                    brief=dict(raw.brief),
                    content_hashes=content_hashes,
                    symbol_ids=symbol_ids,
                    snapshot_time=snapshot_time,
                    # brief_version defaults to time.time_ns() — monotonic
                    # and unique per call, so concurrent writers cannot
                    # collide on the version-guarded upsert.
                )
            )

        store = _resolve_store(context)
        if store is None or not store.is_initialised():
            return None, (
                "submit_atlas: AtlasStore is not initialised; atlas writes "
                "require an active database session factory"
            )

        try:
            store.upsert_chunks(
                project_key=project_key,
                repo_root=repo_root,
                chunks=chunks,
            )
        except Exception as exc:  # narrow failure — surface to the LLM
            logger.exception("submit_atlas: upsert failed")
            return None, f"submit_atlas: upsert failed: {exc}"

        subsystems = [c.subsystem for c in chunks]
        summary_text = (
            f"Atlas updated: wrote {len(chunks)} chunk(s) "
            f"({', '.join(subsystems[:5])}"
            f"{'…' if len(subsystems) > 5 else ''})"
        )
        payload = SubmittedSummary(
            summary=summary_text,
            artifact={
                "project_key": project_key,
                "subsystems": subsystems,
                "rationale": arguments.rationale,
            },
        )
        return payload, None

    def _accepted_message(self, payload: Any) -> str:
        assert isinstance(payload, SubmittedSummary)
        return payload.summary


def _target_paths(brief: dict[str, Any]) -> list[str]:
    raw = brief.get("target_paths") if isinstance(brief, dict) else None
    if not isinstance(raw, list):
        return []
    return [p for p in raw if isinstance(p, str) and p.strip()]


def _resolve_subsystem(
    raw: _SubmitAtlasChunk, idx: int
) -> tuple[str, str | None]:
    """Return ``(subsystem_key, error)`` for one raw chunk."""
    if raw.subsystem:
        return raw.subsystem.strip(), None
    derived = scope_of_artifact(raw.brief)
    if derived:
        return derived, None
    return "", (
        f"submit_atlas: chunk[{idx}] missing subsystem and brief has no "
        "canonical_scope / target_paths to derive one from"
    )


def _collect_symbol_ids(
    context: ToolExecutionContext, file_paths: Any
) -> list[str]:
    """Collect ``"<file>:<symbol>"`` IDs for every file under a chunk's scope.

    Uses the per-run code-intelligence ``SymbolIndex`` attached to
    ``ExecutionMetadata.ci_service``. Missing service / unindexed files
    → empty list; symbol IDs are a best-effort annotation so planners
    can map subsystem → symbols without a live scan, and missing data
    never blocks an atlas write.
    """
    svc = getattr(context.metadata, "ci_service", None)
    if svc is None:
        return []
    symbol_index = getattr(svc, "symbol_index", None)
    if symbol_index is None:
        return []
    out: list[str] = []
    for path in file_paths:
        try:
            symbols = symbol_index.file_symbols(path)
        except Exception:
            continue
        for sym in symbols:
            name = getattr(sym, "name", None)
            if name:
                out.append(f"{path}:{name}")
    return out


def _resolve_store(context: ToolExecutionContext) -> AtlasStore | None:
    """Allow tests to inject an override via ``extras['atlas_store']``."""
    override = context.metadata.extras.get("atlas_store") if hasattr(
        context.metadata, "extras"
    ) else None
    if isinstance(override, AtlasStore):
        return override
    return get_default_store()

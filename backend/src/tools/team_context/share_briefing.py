"""``share_briefing`` — promote a brief into the run-scoped shared context.

Per plan §13:

- Reads ``team_run_id`` from ``context.metadata`` and looks up the live
  ``TeamRun`` from the in-process registry.
- Validates the new ``Briefing`` (XOR enforced by ``__post_init__``).
- Resolves a ``canonical_scope`` key in three layers:
  1. Explicit ``canonical_scope`` field on the loaded artifact, else
  2. Derived from ``artifact["target_paths"]`` via ``canonicalize_scope``, else
  3. The briefing ``name`` (last-resort fallback for inline briefings or
     non-scout artifacts).
- Enforces ``BudgetConfig.max_shared_briefings``; explicit promotions may
  displace a replaceable auto-promoted scout entry before rejecting.
- Treats promotion as a scoped coordination write: if the caller's
  scoped coherence token drifted on the overlapping slice, promotion is
  rejected until live scope is refreshed again.
- Latest-wins replacement on key collision (logged via the result text).
"""

from __future__ import annotations

import logging
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field

from team.context.canonicalize import scope_of_artifact
from team.context.scout_briefings import (
    evict_auto_promoted_scout_briefing,
    scout_artifact_invalidated,
    stamp_shared_briefing_meta,
)
from team.models import Briefing
from team.runtime.registry import get as _get_team_run
from tools.core.base import BaseTool, ToolExecutionContext, ToolResult
from tools.daytona_toolkit.ci_integration import refresh_scope_baseline
from tools.daytona_toolkit.coordination import (
    build_scope_packet_for_context,
    normalize_scope_paths,
    scopes_overlap,
)

logger = logging.getLogger(__name__)


class ShareBriefingInput(BaseModel):
    """Input for the ``share_briefing`` tool."""

    model_config = ConfigDict(
        extra="forbid",
        json_schema_extra={
            "oneOf": [
                {
                    "title": "ArtifactBriefing",
                    "properties": {"source": {"enum": ["artifact"]}},
                    "required": ["ref"],
                },
                {
                    "title": "InlineBriefing",
                    "properties": {"source": {"enum": ["inline"]}},
                    "required": ["inline"],
                },
            ]
        },
    )

    name: str = Field(
        min_length=1,
        description=(
            "Short identifier used as a fallback dedup key and prompt label. "
            "For inline briefings this also becomes the scope key fallback."
        ),
    )
    source: Literal["artifact", "inline"] = Field(
        description=(
            "Where the briefing body comes from. Use \"artifact\" only with a "
            "real stored team artifact ref. Use \"inline\" for a literal "
            "distilled note."
        )
    )
    ref: str | None = Field(
        default=None,
        description="Artifact id when source=\"artifact\".",
    )
    inline: str | None = Field(
        default=None,
        description="Literal note body when source=\"inline\".",
    )
    description: str | None = Field(
        default=None,
        description="Optional one-line hint shown alongside the briefing.",
    )


class ShareBriefingTool(BaseTool):
    """Promote a brief into the run-scoped shared context."""

    name: str = "share_briefing"
    description: str = (
        "Promote a brief into the run-scoped shared context so future "
        "WorkItems and subagents inherit it. Use after reading a brief "
        "with high coverage that you trust will be relevant to siblings. "
        "Treat it like a scoped coordination write: if live coherence on "
        "that slice drifted, refresh first. "
        "If source is \"inline\", you must provide a non-empty inline note. "
        "If source is \"artifact\", you must provide a real team artifact ref."
    )
    input_model: type[BaseModel] = ShareBriefingInput

    async def execute(
        self, arguments: BaseModel, context: ToolExecutionContext
    ) -> ToolResult:
        assert isinstance(arguments, ShareBriefingInput)

        team_run_id = context.metadata.get("team_run_id")
        if not team_run_id:
            return ToolResult(
                output="share_briefing unavailable: no team_run_id in execution context",
                is_error=True,
            )
        team_run = _get_team_run(team_run_id)
        if team_run is None:
            return ToolResult(
                output=f"share_briefing: team_run {team_run_id!r} not registered",
                is_error=True,
            )
        artifact: dict[str, Any] | None = None

        try:
            briefing = Briefing(
                name=arguments.name,
                source=arguments.source,
                ref=arguments.ref,
                inline=arguments.inline,
                description=arguments.description,
            )
        except ValueError as exc:
            detail = f"invalid briefing: {exc}"
            if arguments.source == "artifact" and not arguments.ref:
                detail += (
                    ". `source=\"artifact\"` needs a concrete team artifact ref. "
                    "Use a real stored ref such as an atlas "
                    "`staged_artifact_ref`, a completed WorkItem artifact, or a "
                    "scout `artifact_ref` returned by `run_subagent`; otherwise use "
                    "`source=\"inline\"` with a distilled note or skip promotion."
                )
            if arguments.source == "inline" and not arguments.inline:
                detail += (
                    ". `source=\"inline\"` requires a literal non-empty "
                    "`inline=\"...\"` note. Do not pass null or omit it; either "
                    "provide the distilled note text or skip promotion."
                )
            return ToolResult(output=detail, is_error=True)

        if briefing.source == "artifact":
            assert briefing.ref is not None
            loaded = team_run.artifacts.load(briefing.ref)
            artifact = loaded if isinstance(loaded, dict) else None
            if loaded is None:
                return ToolResult(
                    output=(
                        f"invalid briefing: unknown artifact ref {briefing.ref!r}. "
                        "`share_briefing(source=\"artifact\")` accepts only real "
                        "team artifact refs such as atlas `staged_artifact_ref` "
                        "values, completed WorkItem artifacts, or scout "
                        "`artifact_ref` values returned by `run_subagent`. "
                        "Subagent `run_id` values are audit ids, not shareable "
                        "artifact refs; use `source=\"inline\"` or a real "
                        "artifact ref."
                    ),
                    is_error=True,
                )
            if artifact is None:
                return ToolResult(
                    output=(
                        f"invalid briefing: artifact ref {briefing.ref!r} is not a structured team artifact. "
                        "Use `source=\"inline\"` for distilled notes or promote a real structured artifact."
                    ),
                    is_error=True,
                )
            if scout_artifact_invalidated(team_run.project_context, artifact):
                return ToolResult(
                    output=(
                        f"invalid briefing: scout artifact {briefing.ref!r} predates a same-run "
                        "overlapping edit and is no longer safe to promote. Re-run scout for the "
                        "current scope or distill a fresh inline note instead."
                    ),
                    is_error=True,
                )

        project_ctx = team_run.project_context
        cap = team_run.budgets.max_shared_briefings
        scope_key = _resolve_scope_key(briefing, team_run.artifacts)
        scope_paths = _share_scope_paths(scope_key, artifact)
        pre_share_packet, coherence_error = _preflight_share_scope(
            context,
            scope_paths=scope_paths,
        )
        if coherence_error is not None:
            return ToolResult(
                output=coherence_error,
                is_error=True,
                metadata={
                    "scope_packet": pre_share_packet,
                    "coherence_token": str(pre_share_packet.get("coherence_token") or ""),
                    "scope_key": scope_key,
                },
            )
        replaced = scope_key in project_ctx.shared_briefings
        evicted_scope: str | None = None
        if not replaced and len(project_ctx.shared_briefings) >= cap:
            evicted_scope = evict_auto_promoted_scout_briefing(team_run)
            if evicted_scope is None:
                return ToolResult(
                    output=(
                        f"share_briefing rejected: shared_briefings cap reached "
                        f"({cap}). Existing keys: {sorted(project_ctx.shared_briefings.keys())}"
                    ),
                    is_error=True,
                )

        if replaced:
            logger.info(
                "share_briefing: replacing shared briefing under canonical_scope=%r",
                scope_key,
            )
        project_ctx.shared_briefings[scope_key] = briefing
        stamp_shared_briefing_meta(
            project_ctx,
            scope_key,
            briefing=briefing,
            artifact=artifact,
            source_coherence_token=str(pre_share_packet.get("coherence_token") or ""),
            source_packet_freshness=str(pre_share_packet.get("freshness") or ""),
        )
        project_ctx.auto_promoted_scout_scopes.discard(scope_key)
        post_share_packet = refresh_scope_baseline(context, scope_paths=scope_paths)
        detail = (
            f"shared briefing promoted under scope={scope_key!r} "
            f"(replaced={replaced}, total={len(project_ctx.shared_briefings)})"
        )
        if evicted_scope is not None:
            detail += f"; evicted_auto_promoted_scope={evicted_scope!r}"
        return ToolResult(
            output=detail,
            metadata={
                "scope_key": scope_key,
                "replaced": replaced,
                "evicted_scope": evicted_scope,
                "scope_packet": post_share_packet,
                "coherence_token": str(post_share_packet.get("coherence_token") or ""),
            },
        )


def _resolve_scope_key(briefing: Briefing, artifact_store: Any) -> str:
    """Three-layer resolution: explicit → derived → name fallback."""
    if briefing.source == "artifact" and briefing.ref is not None:
        scope = scope_of_artifact(artifact_store.load(briefing.ref))
        if scope:
            return scope
    return briefing.name


def _share_scope_paths(scope_key: str, artifact: dict[str, Any] | None) -> list[str]:
    if isinstance(artifact, dict):
        target_paths = artifact.get("target_paths")
        if isinstance(target_paths, list):
            return normalize_scope_paths([str(item) for item in target_paths if isinstance(item, str)])
    return normalize_scope_paths([scope_key])


def _should_enforce_share_coherence(
    baseline_packet: dict[str, Any] | None,
    scope_paths: list[str],
) -> bool:
    if not isinstance(baseline_packet, dict) or not scope_paths:
        return False
    baseline_scope_paths = normalize_scope_paths(baseline_packet.get("scope_paths") or [])
    if not baseline_scope_paths:
        return False
    return any(
        scopes_overlap(left, right)
        for left in baseline_scope_paths
        for right in scope_paths
    )


def _preflight_share_scope(
    context: ToolExecutionContext,
    *,
    scope_paths: list[str],
) -> tuple[dict[str, Any], str | None]:
    baseline_packet = context.metadata.get("scope_packet")
    packet = build_scope_packet_for_context(
        context,
        scope_paths=scope_paths,
        baseline_packet=baseline_packet if isinstance(baseline_packet, dict) else None,
    )
    expected = str(context.metadata.get("coherence_token") or "")
    current = str(packet.get("coherence_token") or "")
    if (
        expected
        and current
        and expected != current
        and _should_enforce_share_coherence(
            baseline_packet if isinstance(baseline_packet, dict) else None,
            scope_paths,
        )
    ):
        return packet, (
            "share_briefing rejected: live scope coherence changed on the overlapping slice. "
            "Refresh with `ci_scoped_status(...)` or `inspect_inherited_context(...)` before sharing."
        )
    return packet, None


share_briefing = ShareBriefingTool()

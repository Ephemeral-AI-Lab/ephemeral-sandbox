"""Production ``build_query_context`` callable for the team Executor.

Assembles an agent query context for a ``WorkItem`` by rendering
briefings (shared + dep-snapshotted + explicit) into the preamble of
the initial user message. This is the single prod-side wiring point
for :func:`team.context.briefings.render_briefings`; the same helper
is called from the ``run_subagent`` spawn handler so subagents inherit
``shared_briefings`` symmetrically (§13).
"""

from __future__ import annotations

import json
import logging
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from code_intelligence.routing.service import get_code_intelligence
from team.context.briefings import render_briefings
from team.models import WorkItem, WorkItemStatus
from team.runtime.registry import get as _get_team_run
from tools.core.runtime import ExecutionMetadata
from tools.daytona_toolkit.coordination import (
    build_scope_packet,
    render_scope_packet,
    scope_paths_for_work_item,
)

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)


@dataclass
class TeamAgentContext:
    """Canonical team-runtime context for work and posthook runners."""

    user_message: str = ""
    tool_metadata: ExecutionMetadata = field(default_factory=ExecutionMetadata)
    work_result: Any | None = None
    posthook_metadata_key: str = ""
    posthook_outputs: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if isinstance(self.tool_metadata, dict):
            meta = ExecutionMetadata()
            meta.update(self.tool_metadata)
            self.tool_metadata = meta
        if self.work_result is not None and self.tool_metadata.get("work_result") is None:
            self.tool_metadata["work_result"] = self.work_result
        elif self.work_result is None:
            self.work_result = self.tool_metadata.get("work_result")
        if self.posthook_metadata_key:
            self.tool_metadata["posthook_metadata_key"] = self.posthook_metadata_key
        else:
            self.posthook_metadata_key = self.tool_metadata.get("posthook_metadata_key", "")

    def set_posthook_metadata_key(self, key: str) -> None:
        self.posthook_metadata_key = key
        self.tool_metadata["posthook_metadata_key"] = key

    def set_posthook_output(self, key: str, value: Any) -> None:
        self.posthook_outputs[key] = value
        self.tool_metadata[key] = value

    def get_posthook_output(self, key: str) -> Any:
        if key in self.posthook_outputs:
            return self.posthook_outputs[key]
        return self.tool_metadata.get(key)


def build_work_item_metadata(team_run: TeamRun, wi: WorkItem) -> ExecutionMetadata:
    """Build the canonical routing metadata for a team work item."""
    default_scope_paths = scope_paths_for_work_item(team_run, wi)
    payload = wi.payload if isinstance(wi.payload, dict) else {}
    meta = ExecutionMetadata(
        team_run_id=team_run.id,
        work_item_id=wi.id,
        agent_run_id=wi.agent_run_id,
        agent_name=wi.agent_name,
        sandbox_id=getattr(team_run, "sandbox_id", "") or "",
    )
    # Captured before the agent starts its work phase. Scout artifacts
    # re-use this as their snapshot cutoff so atlas freshness can see
    # edits that land during the scout's read window.
    meta["work_item_started_at"] = time.time()
    meta["retry_count"] = wi.retry_count
    meta["max_retries"] = wi.max_retries
    meta["coordination_mode"] = "ultra"
    meta["require_declared_shell_outputs"] = True
    repo_root = str(getattr(getattr(team_run, "project_context", None), "repo_root", "") or "")
    if repo_root:
        meta["daytona_cwd"] = repo_root
        meta["ci_workspace_root"] = repo_root
    if default_scope_paths:
        meta["default_scope_paths"] = default_scope_paths
    for key in ("owned_files", "owned_failures", "touches_paths", "verify"):
        value = payload.get(key)
        if value not in (None, "", [], {}):
            meta[key] = value
    return meta


def _maybe_attach_live_scope_packet(
    team_run: TeamRun,
    wi: WorkItem,
    *,
    meta: ExecutionMetadata,
    user_message: str,
) -> tuple[ExecutionMetadata, str]:
    """Attach scope metadata and prompt preamble when CI is available."""
    sandbox_id = meta.sandbox_id or ""
    repo_root = str(meta.get("ci_workspace_root") or "")
    if not sandbox_id or not repo_root:
        return meta, user_message

    try:
        ci_service = get_code_intelligence(
            sandbox_id=sandbox_id,
            workspace_root=repo_root,
        )
    except Exception:
        logger.debug(
            "scope packet injection skipped for run=%s wi=%s",
            getattr(team_run, "id", ""),
            getattr(wi, "id", ""),
            exc_info=True,
        )
        return meta, user_message

    scope_packet = build_scope_packet(
        scope_paths=scope_paths_for_work_item(team_run, wi),
        svc=ci_service,
        team_run=team_run,
    )
    meta["scope_packet"] = scope_packet
    coherence_token = str(scope_packet.get("coherence_token") or "")
    if coherence_token:
        meta["coherence_token"] = coherence_token
    rendered_packet = render_scope_packet(scope_packet)
    if rendered_packet:
        user_message = f"{rendered_packet}\n\n{user_message}"
    return meta, user_message


def build_initial_user_message(
    team_run: TeamRun,
    wi: WorkItem,
    base_prompt: str,
) -> str:
    """Prepend rendered briefings (if any) to ``base_prompt``.

    Used by both the DAG executor (via ``build_query_context``) and the
    ``run_subagent`` spawn path so shared/dep/explicit briefings always
    flow into the child's initial user turn.
    """
    preamble = render_briefings(
        wi,
        team_run.dispatcher.artifact_store,
        project_context=getattr(team_run, "project_context", None),
        budgets=team_run.budgets,
    )
    if not preamble:
        return base_prompt
    return f"{preamble}\n\n{base_prompt}"


def prepend_shared_briefings_for_subagent(team_run_id: str | None, body: str) -> str:
    """Inject the team run's ``shared_briefings`` into a subagent prompt.

    Symmetric with the DAG executor path: the same ``render_briefings``
    helper renders the shared-context preamble so subagents inherit
    run-scoped context without re-exploring (§13). Parent ``wi.briefings``
    are deliberately NOT forwarded — only ``shared_briefings`` cross the
    subagent boundary.

    Returns ``body`` unchanged when no team run is registered or no
    shared briefings exist.
    """
    if not team_run_id:
        return body
    team_run = _get_team_run(team_run_id)
    if team_run is None:
        return body
    placeholder = WorkItem(
        id=f"subagent-{team_run_id}",
        team_run_id=team_run_id,
        agent_name="subagent",
        status=WorkItemStatus.RUNNING,
    )
    preamble = render_briefings(
        placeholder,
        team_run.artifacts,
        project_context=team_run.project_context,
        budgets=team_run.budgets,
    )
    if not preamble:
        return body
    return f"{preamble}\n\n{body}"


def default_base_prompt(wi: WorkItem) -> str:
    """Minimal default rendering of a WorkItem payload into a user message."""
    if wi.replan_source_id is not None:
        return _render_replan_prompt(wi)
    payload = wi.payload or {}
    rendered = render_work_item_payload(payload)
    if rendered is not None:
        return rendered
    return f"Execute work item {wi.id} (agent={wi.agent_name}).\nPayload: {payload!r}"


def _render_replan_prompt(wi: WorkItem) -> str:
    """Render a replan work item with full failure context."""
    payload = wi.payload or {}
    original = json.dumps(payload.get("original_payload", {}), indent=2, default=str)
    return (
        f"## Replan Request\n\n"
        f"A sibling work item failed and requires corrective action at this depth level.\n\n"
        f"**Failed work item**: {payload.get('failed_work_item_id', 'unknown')}\n"
        f"**Failed agent**: {payload.get('failed_agent', 'unknown')}\n"
        f"**Failure reason**: {payload.get('failure_reason', 'unknown')}\n\n"
        f"### Failure Context\n{payload.get('failure_context', 'No context provided.')}\n\n"
        f"### Suggestion\n{payload.get('suggestion', 'None')}\n\n"
        f"### Original Payload\n{original}\n\n"
        f"Analyze the failure and return a JSON corrective replan payload with "
        f"``add_items`` and optional ``cancel_ids`` for the posthook agent to submit. "
        f"Completed sibling artifacts are available via dependency briefings above."
    )


def render_work_item_payload(payload: Any) -> str | None:
    """Render a structured payload without dropping critical fields."""
    if isinstance(payload, dict):
        if not payload:
            return None
        rendered_payload = json.dumps(payload, indent=2, default=str)
        primary: list[str] = []
        for key in ("task", "prompt", "description", "instructions", "final_text"):
            value = payload.get(key)
            if isinstance(value, str) and value.strip():
                primary.append(value.strip())
        if primary:
            return (
                "\n\n".join(primary)
                + "\n\nWorkItem payload (authoritative):\n"
                + rendered_payload
            )
        return "WorkItem payload (authoritative):\n" + rendered_payload
    if isinstance(payload, str):
        return payload
    return None


def build_query_context(
    defn: AgentDefinition,  # noqa: ARG001 — kept for QueryContextBuilder signature parity
    team_run: TeamRun,
    wi: WorkItem,
) -> TeamAgentContext:
    """Default production ``QueryContextBuilder``.

    Returns the canonical typed context carrying the rendered user
    message plus routing metadata that downstream hooks and tools rely on.
    Production executor factories may wrap this to add domain-specific
    fields — the briefings-preamble contract lives here.
    """
    meta = build_work_item_metadata(team_run, wi)
    user_message = build_initial_user_message(team_run, wi, default_base_prompt(wi))
    meta, user_message = _maybe_attach_live_scope_packet(
        team_run,
        wi,
        meta=meta,
        user_message=user_message,
    )
    return TeamAgentContext(
        user_message=user_message,
        tool_metadata=meta,
    )

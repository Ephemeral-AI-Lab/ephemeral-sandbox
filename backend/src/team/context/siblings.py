"""Tier 2 — live view of sibling WorkItems inside the same TeamRun."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from team.types import WorkItemStatus

if TYPE_CHECKING:
    from team.dispatcher import Dispatcher


_SUMMARY_CHARS = 200


def _truncate(text: str, n: int = _SUMMARY_CHARS) -> str:
    if len(text) <= n:
        return text
    return text[: n - 3] + "..."


@dataclass
class SiblingSummary:
    work_item_id: str
    agent_name: str
    status: str
    payload_summary: str
    artifact_summary: str


class SiblingView:
    """Thin accessor bound to a specific WorkItem inside one TeamRun."""

    def __init__(self, dispatcher: "Dispatcher", work_item_id: str, artifact_store: Any) -> None:
        self._dispatcher = dispatcher
        self._self_id = work_item_id
        self._artifacts = artifact_store

    def list(self, status: str | None = None) -> list[SiblingSummary]:
        out: list[SiblingSummary] = []
        for wi_id, wi in self._dispatcher.graph.items():
            if wi_id == self._self_id:
                continue
            if status is not None and wi.status.value != status:
                continue
            artifact = self._artifacts.load(wi_id)
            artifact_summary = ""
            if artifact is not None:
                # Prefer the AgentResult-style 'summary' if the artifact has one.
                if isinstance(artifact, dict) and "summary" in artifact:
                    artifact_summary = _truncate(str(artifact["summary"]))
                else:
                    artifact_summary = _truncate(repr(artifact))
            out.append(
                SiblingSummary(
                    work_item_id=wi_id,
                    agent_name=wi.agent_name,
                    status=wi.status.value,
                    payload_summary=_truncate(repr(wi.payload)),
                    artifact_summary=artifact_summary,
                )
            )
        return out

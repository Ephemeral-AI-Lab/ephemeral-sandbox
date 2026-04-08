"""Tier 2 — live view of sibling WorkItems inside the same TeamRun."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from team.dispatcher import Dispatcher


_SUMMARY_CHARS = 200


class SiblingView:
    """Thin accessor bound to a specific WorkItem inside one TeamRun."""

    def __init__(self, dispatcher: "Dispatcher", work_item_id: str, artifact_store: Any) -> None:
        self._dispatcher = dispatcher
        self._self_id = work_item_id
        self._artifacts = artifact_store

    def list(self, status: str | None = None) -> list[dict[str, Any]]:
        out: list[dict[str, Any]] = []
        # Stable order by created_at so LLM-visible output is reproducible.
        items = sorted(
            self._dispatcher.graph.items(),
            key=lambda kv: (kv[1].created_at, kv[0]),
        )
        for wi_id, wi in items:
            if wi_id == self._self_id:
                continue
            if status is not None and wi.status.value != status:
                continue
            artifact = self._artifacts.load(wi_id)
            artifact_summary = ""
            if artifact is not None:
                if isinstance(artifact, dict) and "summary" in artifact:
                    raw = str(artifact["summary"])
                else:
                    raw = repr(artifact)
                artifact_summary = raw if len(raw) <= _SUMMARY_CHARS else raw[: _SUMMARY_CHARS - 3] + "..."
            payload_repr = repr(wi.payload)
            payload_summary = (
                payload_repr if len(payload_repr) <= _SUMMARY_CHARS else payload_repr[: _SUMMARY_CHARS - 3] + "..."
            )
            out.append(
                {
                    "work_item_id": wi_id,
                    "agent_name": wi.agent_name,
                    "status": wi.status.value,
                    "payload_summary": payload_summary,
                    "artifact_summary": artifact_summary,
                }
            )
        return out

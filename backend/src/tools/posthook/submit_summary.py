"""``submit_summary`` tool — stashes a worker's summary+artifact in metadata."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool


@dataclass
class SubmittedSummary:
    """Posthook-validated worker output.

    ``summary`` is the 1-3 sentence gloss peers and the orchestrator
    consume. ``artifact`` is optional structured output (files changed,
    findings, etc.).
    """

    summary: str
    artifact: dict[str, Any] | None = None


class SubmitSummaryInput(BaseModel):
    summary: str = Field(
        ...,
        description=(
            "1-3 sentences describing what you accomplished on this "
            "WorkItem. Peer agents and the orchestrator will read this."
        ),
        min_length=1,
    )
    artifact: dict[str, Any] | None = Field(
        default=None,
        description=(
            "Optional structured output (files changed, findings, ...). "
            "Omit if this work item has nothing structured to return."
        ),
    )


class SubmitSummaryTool(SubmitPosthookTool):
    name: str = "submit_summary"
    description: str = (
        "Submit your WorkItem result. MUST be called exactly once with a "
        "concise summary of what you accomplished; artifact is optional "
        "structured output. The team run fails this WorkItem if you do "
        "not call submit_summary."
    )
    input_model = SubmitSummaryInput
    default_metadata_key: str = "submitted_summary"

    def _build_payload(
        self, arguments: BaseModel, context: ToolExecutionContext  # noqa: ARG002
    ) -> tuple[Any, str | None]:
        assert isinstance(arguments, SubmitSummaryInput)
        summary = arguments.summary.strip()
        if not summary:
            return None, "summary must be non-empty"
        return (
            SubmittedSummary(summary=summary, artifact=arguments.artifact),
            None,
        )

    def _accepted_message(self, payload: Any) -> str:
        assert isinstance(payload, SubmittedSummary)
        return f"Summary accepted ({len(payload.summary)} chars)."

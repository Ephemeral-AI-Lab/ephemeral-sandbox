"""``submit_replan`` tool — stashes a validated ReplanPlan in ``ctx.tool_metadata``."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field, field_validator

from team.models import ReplanPlan, WorkItemKind
from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool, _decode_json_array_string


class _SubmitReplanBriefing(BaseModel):
    name: str
    source: str
    ref: str | None = None
    inline: str | None = None
    description: str | None = None


class _SubmitReplanItem(BaseModel):
    agent_name: str
    payload: dict[str, Any] = Field(default_factory=dict)
    local_id: str | None = None
    deps: list[str] = Field(default_factory=list)
    notes: str | None = None
    timeout_seconds: float | None = None
    kind: WorkItemKind = WorkItemKind.ATOMIC
    briefings: list[_SubmitReplanBriefing] = Field(default_factory=list)


class SubmitReplanInput(BaseModel):
    add_items: list[_SubmitReplanItem] = Field(
        default_factory=list,
        description="New corrective work items to add as siblings at the failed item's depth.",
    )
    cancel_ids: list[str] = Field(
        default_factory=list,
        description="IDs of PENDING/READY work items to cancel (must share same parent).",
    )

    @field_validator("add_items", "cancel_ids", mode="before")
    @classmethod
    def _deserialize_lists(cls, value: Any) -> Any:
        return _decode_json_array_string(value)


class SubmitReplanTool(SubmitPosthookTool):
    name: str = "submit_replan"
    description: str = (
        "Submit a corrective replan: new work items to add at the failed node's "
        "depth level, and/or stale pending items to cancel. Items are inserted "
        "as true siblings (same depth, same parent) of the failed work item. "
        "If validation fails the tool returns a structured error and you MUST "
        "fix it and call submit_replan again."
    )
    input_model = SubmitReplanInput
    default_metadata_key: str = "submitted_replan"

    def _build_payload(
        self, arguments: BaseModel, context: ToolExecutionContext
    ) -> tuple[Any, str | None]:
        assert isinstance(arguments, SubmitReplanInput)
        try:
            replan = ReplanPlan.from_dict(arguments.model_dump())
        except Exception as exc:
            return None, f"Invalid ReplanPlan shape: {exc}"

        if not replan.add_items and not replan.cancel_ids:
            return None, "ReplanPlan must have at least one add_item or cancel_id."

        # Structural validation (local_id uniqueness, etc.)
        seen_locals: set[str] = set()
        for item in replan.add_items:
            if item.local_id is not None:
                if item.local_id in seen_locals:
                    return None, f"Duplicate local_id '{item.local_id}' in add_items."
                seen_locals.add(item.local_id)

        return replan, None

    def _accepted_message(self, payload: Any) -> str:
        assert isinstance(payload, ReplanPlan)
        return (
            f"Replan accepted: {len(payload.add_items)} item(s) to add, "
            f"{len(payload.cancel_ids)} item(s) to cancel."
        )

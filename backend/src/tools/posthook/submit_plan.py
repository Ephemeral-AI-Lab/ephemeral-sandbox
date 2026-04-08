"""``submit_plan`` tool — stashes a validated Plan in ``ctx.tool_metadata``."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from team.models import Plan, WorkItemKind, WorkItemSpec
from team.planning.validation import validate_plan_phase_a
from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool


class _SubmitPlanItem(BaseModel):
    agent_name: str
    payload: dict[str, Any] = Field(default_factory=dict)
    local_id: str | None = None
    deps: list[str] = Field(default_factory=list)
    notes: str | None = None
    timeout_seconds: float | None = None
    kind: WorkItemKind = WorkItemKind.ATOMIC


class SubmitPlanInput(BaseModel):
    items: list[_SubmitPlanItem]
    rationale: str | None = None


class SubmitPlanTool(SubmitPosthookTool):
    name: str = "submit_plan"
    description: str = (
        "Submit a Plan to extend the team's DAG. Each item names an existing "
        "agent and an optional list of dependency local_ids or external "
        "work_item_ids. Validation runs synchronously: if any structural "
        "issue is found the tool returns a structured error and you MUST "
        "fix it and call submit_plan again."
    )
    input_model = SubmitPlanInput
    default_metadata_key: str = "submitted_plan"

    def _build_payload(
        self, arguments: BaseModel, context: ToolExecutionContext
    ) -> tuple[Any, str | None]:
        assert isinstance(arguments, SubmitPlanInput)
        data = arguments.model_dump()
        try:
            plan = Plan(
                items=[
                    WorkItemSpec(
                        agent_name=str(it["agent_name"]),
                        payload=dict(it.get("payload") or {}),
                        local_id=it.get("local_id"),
                        deps=list(it.get("deps") or []),
                        notes=it.get("notes"),
                        timeout_seconds=it.get("timeout_seconds"),
                        kind=WorkItemKind(it.get("kind", "atomic")),
                    )
                    for it in (data.get("items") or [])
                ],
                rationale=data.get("rationale"),
            )
        except Exception as exc:
            return None, f"Invalid Plan shape: {exc}"

        max_plan_size = int(context.metadata.get("max_plan_size", 50) or 50)
        issues = validate_plan_phase_a(plan, max_plan_size=max_plan_size)
        if issues:
            lines = [f"- {i['field']}: {i['msg']}" for i in issues]
            return None, (
                "invalid_plan:\n"
                + "\n".join(lines)
                + "\n\nFix the issues above and call submit_plan again."
            )
        return plan, None

    def _accepted_message(self, payload: Any) -> str:
        assert isinstance(payload, Plan)
        return f"Plan accepted: {len(payload.items)} item(s) queued for dispatch."

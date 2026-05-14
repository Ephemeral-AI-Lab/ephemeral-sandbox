"""Full-system capacity matrix scenario.

This scenario is the capacity-suite facade over the existing full-stack
adversarial flow. It keeps the proven TaskCenter/sandbox/LSP choreography and
adds a capacity-specific metrics artifact before the final release guard.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from live_e2e.scenarios.base import ScenarioContext
from live_e2e.scenarios.full_stack_adversarial import FullStackAdversarial


class FullSystemCapacityMatrix(FullStackAdversarial):
    """Composite capacity run across TaskCenter, sandbox, plugins, and audit."""

    name = "capacity.full_system_capacity_matrix"

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        rendered_prompt = ctx.rendered_prompt or ctx.prompt or ""
        if "ACTION capacity_metrics_full_system" in rendered_prompt:
            return ("capacity_metrics_full_system",)
        return super().executor_actions(ctx)

    def _final_plan(self, ctx: ScenarioContext) -> dict[str, Any]:
        plan = super()._final_plan(ctx)
        tasks = list(plan["tasks"])
        task_specs = dict(plan["task_specs"])

        tasks.append(
            {
                "id": "capacity_metrics_summary",
                "agent_name": "executor",
                "deps": ["final_reconciliation_check"],
            }
        )
        for task in tasks:
            if task["id"] == "final_release_guard":
                task["deps"] = ["capacity_metrics_summary"]
                break

        task_specs["capacity_metrics_summary"] = (
            "ACTION capacity_metrics_full_system profile=project"
        )
        plan["tasks"] = tasks
        plan["task_specs"] = task_specs
        plan["evaluation_criteria"] = [
            *plan["evaluation_criteria"],
            "Capacity metrics artifact uses live_e2e.capacity.v1 schema.",
        ]
        return plan


__all__ = ["FullSystemCapacityMatrix"]

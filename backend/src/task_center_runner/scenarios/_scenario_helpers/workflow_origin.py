"""Workflow-origin predicates used by delegated-workflow scenarios."""

from __future__ import annotations

from task_center_runner.scenarios.base import ScenarioContext


def is_entry_origin_workflow(ctx: ScenarioContext) -> bool:
    """True when the scenario context is still in the root request agent."""
    return ctx.workflow is None


def is_recursive_workflow(ctx: ScenarioContext) -> bool:
    """True when the scenario context is inside a delegated Workflow."""
    return not is_entry_origin_workflow(ctx)


__all__ = ["is_recursive_workflow", "is_entry_origin_workflow"]

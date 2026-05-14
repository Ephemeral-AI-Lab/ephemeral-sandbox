"""OCC concurrent conflict detection — write/edit/conflict round trip.

Reference scenario for the sandbox subsystem. Plan emits one executor task
that fires the existing ``sandbox_integrity`` action, which exercises:

- ``write_file`` → real ``Service.apply_changeset`` → layer published
- ``read_file`` round trip — proves layerstack publish is readable
- ``edit_file`` (search/replace) — proves OCC merge of disjoint edits
- ``shell`` command mutating a file — proves overlay capture path
- batch edit covering multiple search/replace blocks
- a deliberately stale edit that triggers conflict reporting

Asserts on the ``EventType.SANDBOX_*`` events emitted from tool completions:
``SANDBOX_BATCH_EDIT_APPLIED`` and ``SANDBOX_CONFLICT_DETECTED`` must both
appear in the run's event sequence. This is the canonical pattern for
sandbox-subsystem scenarios that need to assert subsystem-level behavior.
"""

from __future__ import annotations

from collections.abc import Sequence

from tools.submission.evaluator import submit_evaluation_success
from tools.submission.planner import submit_full_plan

from live_e2e.audit.events import EventType
from live_e2e.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


_INTEGRITY_PLAN = {
    "task_specification": (
        "Drive the sandbox toolkit through write, read, edit, shell, batch "
        "edit, and a stale-edit conflict to exercise OCC + overlay + "
        "layerstack."
    ),
    "evaluation_criteria": [
        "Sandbox toolkit can read, write, edit, and run shell.",
        "Batch edit succeeds and a stale edit reports conflict.",
    ],
    "tasks": [
        {"id": "sandbox_integrity", "agent_name": "executor", "deps": []},
    ],
    "task_specs": {
        "sandbox_integrity": (
            "Exercise the sandbox filesystem with write_file, read_file, "
            "edit_file, shell, a batch public edit, and an expected conflict."
        ),
    },
}


class OccConcurrentConflicts(ScenarioBase):
    """OCC + layer-stack + overlay + conflict round trip via sandbox_integrity."""

    name = "sandbox.occ_concurrent_conflicts"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.ENTRY_EXECUTOR_INVOKED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.SANDBOX_BATCH_EDIT_APPLIED,
        EventType.SANDBOX_CONFLICT_DETECTED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(submit_full_plan, dict(_INTEGRITY_PLAN))

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:  # noqa: ARG002
        return ("sandbox_integrity",)

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": (
                    "Sandbox integrity probe captured both batch-edit and "
                    "conflict evidence."
                ),
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


__all__ = ["OccConcurrentConflicts"]

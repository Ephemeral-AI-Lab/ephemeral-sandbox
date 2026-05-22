"""EventType enum + Event dataclass for the in-memory audit bus.

Events live in-memory only — they drive hook dispatch and metrics aggregation.
There is no persisted ``events.jsonl``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from typing import Any

from task_center_runner.audit.node_id import NodeId


class EventType(StrEnum):
    """All audit event kinds. Plan §8."""

    # task center lifecycle
    RUN_STARTED = "run_started"
    RUN_COMPLETED = "run_completed"
    GOAL_STARTED = "goal_started"
    GOAL_COMPLETED = "goal_completed"
    GOAL_REQUESTED = "goal_requested"
    ITERATION_STARTED = "iteration_started"
    ITERATION_COMPLETED = "iteration_completed"
    ITERATION_FROM_DEFERRED_GOAL_CREATED = "iteration_continuation_created"
    ATTEMPT_STARTED = "attempt_started"
    ATTEMPT_PASSED = "attempt_passed"
    ATTEMPT_FAILED = "attempt_failed"

    # agent invocations
    PLANNER_INVOKED = "planner_invoked"
    PLANNER_COMPLETES_GOAL_PLAN = "planner_full_plan"
    PLANNER_DEFERS_GOAL_PLAN = "planner_partial_plan"
    PLANNER_REPLAN = "planner_replan"
    EXECUTOR_INVOKED = "executor_invoked"
    EXECUTOR_SUCCESS = "executor_success"
    EXECUTOR_FAILURE = "executor_failure"
    VERIFIER_INVOKED = "verifier_invoked"
    VERIFIER_SUCCESS = "verifier_success"
    VERIFIER_FAILURE = "verifier_failure"
    EVALUATOR_INVOKED = "evaluator_invoked"
    EVALUATOR_SUCCESS = "evaluator_success"
    EVALUATOR_FAILURE = "evaluator_failure"
    RECURSIVE_GOAL_REQUESTED = "recursive_goal_requested"
    RECURSIVE_GOAL_COMPLETED = "recursive_goal_completed"
    FULL_STACK_SCRIPT_COMPLETED = "full_stack_script_completed"

    # tools
    TOOL_CALL_STARTED = "tool_call_started"
    TOOL_CALL_COMPLETED = "tool_call_completed"
    TOOL_CALL_ERROR = "tool_call_error"

    # sandbox-derived
    SANDBOX_WRITE_COMMITTED = "sandbox_write_committed"
    SANDBOX_EDIT_COMMITTED = "sandbox_edit_committed"
    SANDBOX_SHELL_COMMITTED = "sandbox_shell_committed"
    SANDBOX_BATCH_EDIT_APPLIED = "sandbox_batch_edit_applied"
    SANDBOX_CONFLICT_DETECTED = "sandbox_conflict_detected"
    SANDBOX_LAYER_STACK_LEASE_ACQUIRED = "sandbox_layer_stack_lease_acquired"
    SANDBOX_LAYER_STACK_LAYER_CREATED = "sandbox_layer_stack_layer_created"
    SANDBOX_LAYER_STACK_LAYERS_SQUASHED = "sandbox_layer_stack_layers_squashed"
    SANDBOX_OVERLAY_EXECUTED = "sandbox_overlay_executed"
    SANDBOX_OCC_CHANGESET_RECEIVED = "sandbox_occ_changeset_received"
    SANDBOX_OCC_CHANGES_COMMITTED = "sandbox_occ_changes_committed"
    SANDBOX_RESOURCE_SNAPSHOT = "sandbox_resource_snapshot"
    SANDBOX_SHELL_LAUNCHED = "sandbox_shell_launched"
    SANDBOX_SHELL_CANCELLED = "sandbox_shell_cancelled"
    SANDBOX_SHELL_REAPED = "sandbox_shell_reaped"

    # hook synthetic
    HOOK_INJECTED_FAILURE = "hook_injected_failure"
    HOOK_ASSERTED = "hook_asserted"

    # mock-runner side-channel events. Emitted only by MockSquadRunner under
    # the mock-scenario pipeline (Phase 4); consumed only by the legacy
    # live_e2e/runner.py shim to reconstruct the rich RunReport view.
    # Removed in the next milestone together with the shim — real-agent and
    # benchmark runs never emit them, so subscribers must tolerate absence.
    MOCK_LAUNCH_RECORDED = "mock_launch_recorded"
    MOCK_TOOL_CALL_RECORDED = "mock_tool_call_recorded"
    MOCK_PROMPT_INSPECTED = "mock_prompt_inspected"
    MOCK_SANDBOX_CHECK_RECORDED = "mock_sandbox_check_recorded"


@dataclass(frozen=True, slots=True)
class Event:
    """One audit event."""

    type: EventType
    node: NodeId
    payload: dict[str, Any] = field(default_factory=dict)
    correlation_id: str | None = None
    ts: datetime = field(default_factory=lambda: datetime.now(UTC))


__all__ = ["Event", "EventType"]

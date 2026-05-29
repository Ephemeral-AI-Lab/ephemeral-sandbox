"""EventType enum + Event dataclass for the in-memory audit bus.

Events live in-memory only — they drive hook dispatch and metrics aggregation.
There is no persisted ``events.jsonl``.

Phase-timing contract (PLAN §14, isolated_workspace tier)
---------------------------------------------------------

The five ``sandbox_isolated_workspace_*`` events carry two additive payload
fields used for per-operation latency analysis:

- ``total_ms`` (float): wall-clock cost of the operation.
- ``phases_ms`` (dict[str, float]): per-phase breakdown.

Two rules every future emitter MUST respect:

1. **Conditional-key emission.** A phase appears in ``phases_ms`` only when
   that codepath actually ran to completion. Emitting ``"<phase>": 0.0`` for
   a stubbed or skipped branch is FORBIDDEN — absence and zero have distinct
   semantics.
2. **SUBSET-COVER invariant.** For every emitted event,
   ``sum(phases_ms.values()) <= total_ms + max(2.0, 0.05 * total_ms)``. The
   inequality is one-sided because conditional-key emission means
   ``sum(phases_ms.values())`` is strictly ``<= total_ms`` (plus a small
   bookkeeping ε to absorb the timer's own overhead).

These rules are pinned by the live tests under
``task_center_runner/tests/mock/sandbox/isolated_workspace/performance/``.
Aggregators (``performance_report.py``) consume ``total_ms`` and the
``phases_ms`` dict opaquely — they should never assume any specific phase
key is present.
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
    WORKFLOW_STARTED = "workflow_started"
    WORKFLOW_COMPLETED = "workflow_completed"
    WORKFLOW_REQUESTED = "workflow_requested"
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
    RECURSIVE_WORKFLOW_REQUESTED = "recursive_workflow_requested"
    RECURSIVE_WORKFLOW_COMPLETED = "recursive_workflow_completed"
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
    SANDBOX_OVERLAY_EXECUTED = "pipeline_executed"
    SANDBOX_OCC_CHANGESET_RECEIVED = "sandbox_occ_changeset_received"
    SANDBOX_OCC_CHANGES_COMMITTED = "sandbox_occ_changes_committed"
    SANDBOX_RESOURCE_SNAPSHOT = "sandbox_resource_snapshot"
    SANDBOX_TOOL_CANCELLED = "sandbox_tool_cancelled"
    SANDBOX_ISOLATED_WORKSPACE_ENTER = "sandbox_isolated_workspace_enter"
    SANDBOX_ISOLATED_WORKSPACE_EXIT = "sandbox_isolated_workspace_exit"
    SANDBOX_ISOLATED_WORKSPACE_TOOL_CALL = "sandbox_isolated_workspace_tool_call"
    SANDBOX_ISOLATED_WORKSPACE_EVICTED = "sandbox_isolated_workspace_evicted"
    SANDBOX_ISOLATED_WORKSPACE_GC_ORPHAN = "sandbox_isolated_workspace_gc_orphan"

    # hook synthetic
    HOOK_INJECTED_FAILURE = "hook_injected_failure"
    HOOK_ASSERTED = "hook_asserted"

    # mock-runner side-channel events. Emitted only by MockSquadRunner under
    # the mock-scenario pipeline; ScenarioLifecycle accumulates them for the
    # rich RunReport view. Real-agent and benchmark runs never emit them, so
    # subscribers must tolerate absence.
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

"""Sandbox audit event type constants."""

from __future__ import annotations

from enum import Enum

# Public sandbox operation lifecycle.
OPERATION_STARTED = "sandbox.operation.started"
OPERATION_COMPLETED = "sandbox.operation.completed"
OPERATION_FAILED = "sandbox.operation.failed"
OPERATION_CONFLICTED = "sandbox.operation.conflicted"
OPERATION_EVENTS = (
    OPERATION_STARTED,
    OPERATION_COMPLETED,
    OPERATION_FAILED,
    OPERATION_CONFLICTED,
)

# Timing-derived OCC facts.
OCC_PREPARED = "sandbox.occ.prepared"
OCC_COMMITTED = "sandbox.occ.committed"
OCC_CONFLICTED = "sandbox.occ.conflicted"
OCC_EVENTS = (OCC_PREPARED, OCC_COMMITTED, OCC_CONFLICTED)

# Timing-derived overlay facts.
OVERLAY_EXECUTED = "sandbox.overlay.executed"
OVERLAY_EVENTS = (OVERLAY_EXECUTED,)

# Timing-derived layer-stack facts.
LAYER_STACK_LEASE_ACQUIRED = "sandbox.layer_stack.lease_acquired"
LAYER_STACK_LAYER_PUBLISHED = "sandbox.layer_stack.layer_published"
LAYER_STACK_AUTO_SQUASHED = "sandbox.layer_stack.auto_squashed"
LAYER_STACK_EVENTS = (
    LAYER_STACK_LEASE_ACQUIRED,
    LAYER_STACK_LAYER_PUBLISHED,
    LAYER_STACK_AUTO_SQUASHED,
)

# Timing-derived resource facts.
RESOURCE_SNAPSHOT = "sandbox.resource.snapshot"
RESOURCE_EVENTS = (RESOURCE_SNAPSHOT,)
SUBSYSTEM_EVENTS = OCC_EVENTS + OVERLAY_EVENTS + LAYER_STACK_EVENTS + RESOURCE_EVENTS

# Host-side workspace lifecycle wrappers.
WORKSPACE_LIFECYCLE_STARTED = "workspace_lifecycle_started"
WORKSPACE_LIFECYCLE_COMPLETED = "workspace_lifecycle_completed"
WORKSPACE_LIFECYCLE_FAILED = "workspace_lifecycle_failed"
# Phase 4: engine-side rejection when a Intent.LIFECYCLE tool is co-batched
# with other tool calls. Routed through the same lifecycle audit path so
# the rejection appears in trace bundles next to enter/exit events.
WORKSPACE_LIFECYCLE_BATCH_REJECTED = "workspace_lifecycle_batch_rejected"
WORKSPACE_LIFECYCLE_EVENTS = (
    WORKSPACE_LIFECYCLE_STARTED,
    WORKSPACE_LIFECYCLE_COMPLETED,
    WORKSPACE_LIFECYCLE_FAILED,
    WORKSPACE_LIFECYCLE_BATCH_REJECTED,
)


class IsolatedWorkspaceAuditEvent(str, Enum):
    """Daemon-side isolated-workspace audit event types."""

    ENTER = "sandbox_isolated_workspace_enter"
    EXIT = "sandbox_isolated_workspace_exit"
    TOOL_CALL = "sandbox_isolated_workspace_tool_call"
    EVICTED = "sandbox_isolated_workspace_evicted"
    GC_ORPHAN = "sandbox_isolated_workspace_gc_orphan"


ISOLATED_WORKSPACE_EVENTS = tuple(event.value for event in IsolatedWorkspaceAuditEvent)

EVENT_FAMILIES = {
    "operation": OPERATION_EVENTS,
    "occ": OCC_EVENTS,
    "overlay": OVERLAY_EVENTS,
    "layer_stack": LAYER_STACK_EVENTS,
    "resource": RESOURCE_EVENTS,
    "workspace_lifecycle": WORKSPACE_LIFECYCLE_EVENTS,
    "isolated_workspace": ISOLATED_WORKSPACE_EVENTS,
}
ALL_EVENT_TYPES = tuple(
    event_type
    for family_events in EVENT_FAMILIES.values()
    for event_type in family_events
)

TIMING_SIGNAL_EVENTS = {
    "occ_prepared": OCC_PREPARED,
    "occ_committed": OCC_COMMITTED,
    "occ_conflicted": OCC_CONFLICTED,
    "overlay_executed": OVERLAY_EXECUTED,
    "layer_stack_lease_acquired": LAYER_STACK_LEASE_ACQUIRED,
    "layer_stack_layer_published": LAYER_STACK_LAYER_PUBLISHED,
    "layer_stack_auto_squashed": LAYER_STACK_AUTO_SQUASHED,
    "resource_snapshot": RESOURCE_SNAPSHOT,
}

__all__ = [
    "ALL_EVENT_TYPES",
    "EVENT_FAMILIES",
    "ISOLATED_WORKSPACE_EVENTS",
    "LAYER_STACK_AUTO_SQUASHED",
    "LAYER_STACK_EVENTS",
    "LAYER_STACK_LAYER_PUBLISHED",
    "LAYER_STACK_LEASE_ACQUIRED",
    "OCC_COMMITTED",
    "OCC_CONFLICTED",
    "OCC_EVENTS",
    "OCC_PREPARED",
    "OPERATION_COMPLETED",
    "OPERATION_CONFLICTED",
    "OPERATION_EVENTS",
    "OPERATION_FAILED",
    "OPERATION_STARTED",
    "OVERLAY_EVENTS",
    "OVERLAY_EXECUTED",
    "RESOURCE_EVENTS",
    "RESOURCE_SNAPSHOT",
    "SUBSYSTEM_EVENTS",
    "TIMING_SIGNAL_EVENTS",
    "WORKSPACE_LIFECYCLE_BATCH_REJECTED",
    "WORKSPACE_LIFECYCLE_COMPLETED",
    "WORKSPACE_LIFECYCLE_EVENTS",
    "WORKSPACE_LIFECYCLE_FAILED",
    "WORKSPACE_LIFECYCLE_STARTED",
    "IsolatedWorkspaceAuditEvent",
]

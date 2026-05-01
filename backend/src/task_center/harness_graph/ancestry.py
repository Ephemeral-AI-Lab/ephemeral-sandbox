"""Canonical ancestor walks across the request → segment → graph → task chain.

This module owns the **single canonical implementation** of the partial-plan
ancestor predicate. Surviving call sites (resolver predicate +
``ResolverContext.has_partial_planned_caller_ancestor`` convenience method)
are one-line shims around this function. The legacy
``PartialPlanAncestorGate`` prehook + ``recursive_partial_plan`` notification
trigger were removed in US-016 — the gate now lives in the agent.md
``terminals:`` filter on ``planner_full_only``.
"""

from __future__ import annotations

from db.stores.complex_task_request_store import ComplexTaskRequestStore
from db.stores.harness_graph_store import HarnessGraphStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.task_segment_store import TaskSegmentStore
from task_center.exceptions import GraphInvariantViolation


def has_partial_planned_caller_ancestor(
    *,
    request_id: str,
    request_store: ComplexTaskRequestStore,
    segment_store: TaskSegmentStore,
    graph_store: HarnessGraphStore,
    task_store: TaskCenterStore,
) -> bool:
    """Return True iff any caller graph in the ancestry submitted a partial plan.

    Walks ``parent_task → parent_graph → parent_segment → parent_request``
    upward from ``request_id`` until a partial-planned caller is found
    (``parent_graph.continuation_goal`` is non-null) or the chain terminates
    (top-level entry executor — no caller graph).

    Raises :class:`GraphInvariantViolation` on cycles and on missing
    intermediate rows once the chain has begun. A missing parent task or a
    parent task with no ``task_center_harness_graph_id`` terminates the walk
    cleanly (top-level case).
    """
    seen_request_ids: set[str] = set()
    current_request_id = request_id

    while True:
        if current_request_id in seen_request_ids:
            raise GraphInvariantViolation(
                "Cycle detected while resolving complex task request ancestry."
            )
        seen_request_ids.add(current_request_id)

        current_request = request_store.get(current_request_id)
        if current_request is None:
            raise GraphInvariantViolation(
                f"ComplexTaskRequest {current_request_id!r} was not found."
            )

        parent_task = task_store.get_task(current_request.requested_by_task_id)
        if parent_task is None:
            return False

        parent_graph_id = str(parent_task.get("task_center_harness_graph_id") or "")
        if not parent_graph_id:
            return False

        parent_graph = graph_store.get(parent_graph_id)
        if parent_graph is None:
            raise GraphInvariantViolation(
                f"Parent HarnessGraph {parent_graph_id!r} was not found."
            )

        if parent_graph.continuation_goal is not None:
            return True

        parent_segment = segment_store.get(parent_graph.task_segment_id)
        if parent_segment is None:
            raise GraphInvariantViolation(
                f"Parent TaskSegment {parent_graph.task_segment_id!r} was not found."
            )

        current_request_id = parent_segment.complex_task_request_id

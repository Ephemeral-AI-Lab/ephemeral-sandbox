"""Harness-graph readiness check used by the dispatcher to promote evaluators."""

from __future__ import annotations

from task_center.graph.store import TaskGraph
from task_center.model import HarnessGraphId, Status


_TERMINAL_STATUSES: frozenset[Status] = frozenset({Status.DONE, Status.FAILED})


def is_harness_graph_ready_for_evaluation(
    graph: TaskGraph, graph_id: HarnessGraphId
) -> bool:
    """True iff every generator in the harness graph is terminal and an evaluator exists.

    Stage 3 introduces verifier nodes in DAGs, so the readiness check now
    iterates ``dag_nodes`` (the union of executors + verifiers). The legacy
    ``executor_task_ids`` field is kept in sync at materialization time, so
    pre-Stage-3 fixtures that populate only the legacy slot still work as a
    fallback when ``dag_nodes`` is empty.
    """
    harness = graph.get_harness_graph(graph_id)
    if harness.evaluator_task_id is None:
        return False
    nodes = harness.dag_nodes if harness.dag_nodes else harness.executor_task_ids
    for tid in nodes:
        if graph.get(tid).status not in _TERMINAL_STATUSES:
            return False
    return True

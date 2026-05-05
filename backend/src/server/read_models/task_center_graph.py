"""TaskCenter request/segment/graph read model."""

from __future__ import annotations

from collections import defaultdict
from typing import Any

from db.stores import (
    ComplexTaskRequestStore,
    HarnessGraphStore,
    TaskCenterStore,
    TaskSegmentStore,
)
from task_center.mission.mission import ComplexTaskRequest
from task_center.attempt import HarnessGraph
from task_center.episode.episode import TaskSegment


def build_task_center_graph_response(
    *,
    task_center_run_id: str,
    request_store: ComplexTaskRequestStore,
    segment_store: TaskSegmentStore,
    graph_store: HarnessGraphStore,
    task_store: TaskCenterStore,
) -> dict[str, Any]:
    """Build the nested graph response without per-node store queries."""
    requests = request_store.list_for_run(task_center_run_id)
    segments = segment_store.list_for_requests([r.id for r in requests])
    graphs = graph_store.list_for_segments([s.id for s in segments])
    tasks = task_store.list_tasks_for_harness_graphs([g.id for g in graphs])

    segments_by_request_id: dict[str, list[TaskSegment]] = defaultdict(list)
    for segment in segments:
        segments_by_request_id[segment.complex_task_request_id].append(segment)

    graphs_by_segment_id: dict[str, list[HarnessGraph]] = defaultdict(list)
    for graph in graphs:
        graphs_by_segment_id[graph.task_segment_id].append(graph)

    tasks_by_graph_id: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for task in tasks:
        graph_id = str(task.get("task_center_harness_graph_id") or "")
        if graph_id:
            tasks_by_graph_id[graph_id].append(task)

    flat_index: list[dict[str, str]] = []
    nested_requests = [
        _request_to_dict(
            request,
            _nested_segments(
                request=request,
                segments_by_request_id=segments_by_request_id,
                graphs_by_segment_id=graphs_by_segment_id,
                tasks_by_graph_id=tasks_by_graph_id,
                flat_index=flat_index,
            ),
        )
        for request in requests
    ]
    return {
        "complex_task_requests": nested_requests,
        "harness_graphs_index": flat_index,
    }


def _nested_segments(
    *,
    request: ComplexTaskRequest,
    segments_by_request_id: dict[str, list[TaskSegment]],
    graphs_by_segment_id: dict[str, list[HarnessGraph]],
    tasks_by_graph_id: dict[str, list[dict[str, Any]]],
    flat_index: list[dict[str, str]],
) -> list[dict[str, Any]]:
    nested_segments: list[dict[str, Any]] = []
    for segment in segments_by_request_id.get(request.id, []):
        nested_graphs = []
        for graph in graphs_by_segment_id.get(segment.id, []):
            nested_graphs.append(
                _graph_to_dict(graph, tasks_by_graph_id.get(graph.id, []))
            )
            flat_index.append(
                {
                    "harness_graph_id": graph.id,
                    "complex_task_request_id": request.id,
                    "task_segment_id": segment.id,
                }
            )
        nested_segments.append(_segment_to_dict(segment, nested_graphs))
    return nested_segments


def _request_to_dict(
    request: ComplexTaskRequest, nested_segments: list[dict[str, Any]]
) -> dict[str, Any]:
    return {
        "id": request.id,
        "task_center_run_id": request.task_center_run_id,
        "requested_by_task_id": request.requested_by_task_id,
        "status": request.status.value,
        "goal": request.goal,
        "final_outcome": request.final_outcome,
        "created_at": request.created_at.isoformat() if request.created_at else None,
        "closed_at": request.closed_at.isoformat() if request.closed_at else None,
        "task_segments": nested_segments,
    }


def _segment_to_dict(
    segment: TaskSegment, nested_graphs: list[dict[str, Any]]
) -> dict[str, Any]:
    return {
        "id": segment.id,
        "sequence_no": segment.sequence_no,
        "creation_reason": segment.creation_reason.value,
        "status": segment.status.value,
        "goal": segment.goal,
        "continuation_goal": segment.continuation_goal,
        "attempt_budget": segment.attempt_budget,
        "created_at": segment.created_at.isoformat() if segment.created_at else None,
        "closed_at": segment.closed_at.isoformat() if segment.closed_at else None,
        "harness_graphs": nested_graphs,
    }


def _graph_to_dict(graph: HarnessGraph, tasks: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "id": graph.id,
        "graph_sequence_no": graph.graph_sequence_no,
        "stage": graph.stage.value,
        "status": graph.status.value,
        "planner_task_id": graph.planner_task_id,
        "task_specification": graph.task_specification,
        "evaluation_criteria": list(graph.evaluation_criteria),
        "generator_task_ids": list(graph.generator_task_ids),
        "evaluator_task_id": graph.evaluator_task_id,
        "continuation_goal": graph.continuation_goal,
        "fail_reason": graph.fail_reason.value if graph.fail_reason is not None else None,
        "created_at": graph.created_at.isoformat() if graph.created_at else None,
        "updated_at": graph.updated_at.isoformat() if graph.updated_at else None,
        "closed_at": graph.closed_at.isoformat() if graph.closed_at else None,
        "tasks": tasks,
    }

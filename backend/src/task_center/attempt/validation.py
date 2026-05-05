"""HarnessGraph-layer invariants. All raise ``GraphInvariantViolation``."""

from __future__ import annotations

from typing import Any

from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.state import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)
from task_center.episode.episode import TaskSegment
from task_center.task import HarnessTaskRole


def assert_graph_sequence_contiguous(
    segment: TaskSegment, new_sequence_no: int
) -> None:
    expected = len(segment.harness_graph_ids) + 1
    if new_sequence_no != expected:
        raise GraphInvariantViolation(
            f"HarnessGraph graph_sequence_no must be contiguous: expected "
            f"{expected}, got {new_sequence_no}"
        )


def assert_fail_reason_present_on_failure(graph: HarnessGraph) -> None:
    if graph.status == HarnessGraphStatus.FAILED and graph.fail_reason is None:
        raise GraphInvariantViolation(
            f"HarnessGraph {graph.id!r} closed FAILED with no fail_reason"
        )


def assert_graph_stage(
    graph: HarnessGraph, expected: HarnessGraphStage
) -> None:
    if graph.stage != expected:
        raise GraphInvariantViolation(
            f"HarnessGraph {graph.id!r} expected stage {expected.value!r}, "
            f"got {graph.stage.value!r}"
        )


def assert_graph_not_closed(graph: HarnessGraph) -> None:
    if graph.is_closed:
        raise GraphInvariantViolation(
            f"HarnessGraph {graph.id!r} is already closed"
        )


def assert_valid_graph_close(
    *,
    status: HarnessGraphStatus,
    fail_reason: HarnessGraphFailReason | None,
) -> None:
    if status == HarnessGraphStatus.FAILED and fail_reason is None:
        raise GraphInvariantViolation("Failed attempt close requires fail_reason")
    if status == HarnessGraphStatus.PASSED and fail_reason is not None:
        raise GraphInvariantViolation("Passed graph close cannot have fail_reason")
    if status == HarnessGraphStatus.RUNNING:
        raise GraphInvariantViolation("Cannot close graph with running status")


def assert_task_belongs_to_graph(
    task: dict[str, Any], graph: HarnessGraph
) -> None:
    if task.get("task_center_harness_graph_id") != graph.id:
        raise GraphInvariantViolation(
            f"Task {task.get('id')!r} does not belong to HarnessGraph "
            f"{graph.id!r}"
        )


def assert_generator_task_for_submission(
    task: dict[str, Any], graph: HarnessGraph
) -> None:
    assert_task_belongs_to_graph(task, graph)
    if task.get("role") != HarnessTaskRole.GENERATOR.value:
        raise GraphInvariantViolation(
            f"Task {task.get('id')!r} is not a generator task"
        )


def assert_evaluator_task_for_submission(
    task: dict[str, Any], graph: HarnessGraph
) -> None:
    assert_task_belongs_to_graph(task, graph)
    if task.get("role") != HarnessTaskRole.EVALUATOR.value:
        raise GraphInvariantViolation(
            f"Task {task.get('id')!r} is not an evaluator task"
        )

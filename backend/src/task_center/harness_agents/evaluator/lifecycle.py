"""Evaluator lifecycle and harness-graph closure operations."""

from __future__ import annotations

from typing import TYPE_CHECKING

from task_center.errors import TaskCenterError
from task_center.model import HarnessGraphId, Status, Task, TaskId, TaskSummary
from task_center.summaries import latest_summary_text

if TYPE_CHECKING:
    from task_center.runtime.orchestrator import TaskCenter


def submit_task_success(tc: "TaskCenter", task_id: TaskId, summary: str) -> None:
    """Mark an evaluator done and close its harness graph successfully."""
    task = tc.graph.get(task_id)
    if task.role != "evaluator":
        raise TaskCenterError(
            f"submit_task_success: task {task_id!r} role {task.role!r} not allowed"
        )
    task.summaries.append(
        TaskSummary(kind="success", text=summary, source_task_id=task_id)
    )
    tc._mark_terminal(task, Status.DONE)
    assert task.task_center_harness_graph_id is not None
    close_harness_graph_success(tc, task.task_center_harness_graph_id, task_id)
    tc._persist_all()
    tc._wakeup.set()


def submit_evaluation_failure(tc: "TaskCenter", task_id: TaskId, summary: str) -> None:
    """Mark an evaluator failed and close its harness graph as failed."""
    task = tc.graph.get(task_id)
    if task.role != "evaluator":
        raise TaskCenterError(
            f"submit_evaluation_failure: task {task_id!r} role {task.role!r} "
            "is not evaluator"
        )
    task.summaries.append(
        TaskSummary(kind="evaluation_failure", text=summary, source_task_id=task_id)
    )
    tc._mark_terminal(task, Status.FAILED)
    assert task.task_center_harness_graph_id is not None
    close_harness_graph_failed(tc, task.task_center_harness_graph_id, task_id)
    tc._persist_all()
    tc._wakeup.set()


def close_harness_graph_success(
    tc: "TaskCenter", graph_id: HarnessGraphId, source_task_id: TaskId
) -> None:
    """Close a harness graph successfully and propagate to its parent task."""
    graph = tc.graph.get_harness_graph(graph_id)
    planner = tc.graph.get(graph.planner_task_id)
    tc._mark_terminal(planner, Status.DONE)
    source_task = tc.graph.get(source_task_id)
    parent = tc.graph.get(graph.root_task_id)
    parent.summaries.append(
        TaskSummary(
            kind="child_success",
            text=latest_summary_text(source_task) or "",
            source_task_id=source_task_id,
        )
    )
    tc._mark_terminal(parent, Status.DONE)
    propagate_parent_terminal(tc, parent, success=True)


def close_harness_graph_failed(
    tc: "TaskCenter", graph_id: HarnessGraphId, source_task_id: TaskId
) -> None:
    """Close a harness graph as failed and propagate to its parent task."""
    graph = tc.graph.get_harness_graph(graph_id)
    planner = tc.graph.get(graph.planner_task_id)
    tc._mark_terminal(planner, Status.FAILED)
    source_task = tc.graph.get(source_task_id)
    parent = tc.graph.get(graph.root_task_id)
    parent.summaries.append(
        TaskSummary(
            kind="child_failure",
            text=latest_summary_text(source_task) or "",
            source_task_id=source_task_id,
        )
    )
    tc._mark_terminal(parent, Status.FAILED)
    propagate_parent_terminal(tc, parent, success=False)


def propagate_parent_terminal(tc: "TaskCenter", parent: Task, *, success: bool) -> None:
    """Bubble a terminal parent task across nested harness graph boundaries."""
    if parent.task_center_harness_graph_id is None:
        return
    if parent.role == "evaluator":
        if success:
            close_harness_graph_success(tc, parent.task_center_harness_graph_id, parent.id)
        else:
            close_harness_graph_failed(tc, parent.task_center_harness_graph_id, parent.id)
    else:
        tc._notify_child_terminal_changed()


def handle_silent_termination(tc: "TaskCenter", task: Task, reason: str) -> None:
    """Treat a silent evaluator exit as a graph-closing evaluation failure."""
    submit_evaluation_failure(tc, task.id, reason)

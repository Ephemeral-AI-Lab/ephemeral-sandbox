"""Build a ``PlannerLaunchContext`` from the current task graph topology."""

from __future__ import annotations

from task_center.graph.store import TaskGraph
from task_center.model import Task, TaskSummary
from task_center.planning.launch_context import PlannerLaunchContext
from task_center.summaries import child_summary_groups


def build_planner_launch_context(
    graph: TaskGraph, caller: Task, task_detail: str
) -> PlannerLaunchContext:
    """Assemble the planner input for a caller that just invoked ``launch_plan_handoff``."""
    if caller.role not in ("executor", "evaluator"):
        raise ValueError(
            "build_planner_launch_context requires an executor or evaluator caller"
        )
    upstream: list[TaskSummary] = []
    prior_handoff: list[TaskSummary] = []
    completed: list[TaskSummary] = []
    failed: list[TaskSummary] = []
    blocked: list[TaskSummary] = []
    requested_goal = caller.input

    if caller.task_center_harness_graph_id is not None:
        harness = graph.get_harness_graph(caller.task_center_harness_graph_id)
        requested_goal = graph.get(harness.parent_task_id).input
        outer_planner = graph.get(harness.planner_task_id)
        upstream = [s for s in outer_planner.summaries if s.kind == "handoff"]
        prior_handoff = list(upstream)
        for tid in harness.executor_task_ids:
            child = graph.get(tid)
            child_completed, child_failed, child_blocked = child_summary_groups(child)
            completed.extend(child_completed)
            failed.extend(child_failed)
            blocked.extend(child_blocked)

    return PlannerLaunchContext(
        task_detail=task_detail,
        caller_task_id=caller.id,
        caller_role=caller.role,
        caller_input=caller.input,
        requested_goal=requested_goal,
        upstream_handoff_summaries=upstream,
        prior_planner_handoff=prior_handoff,
        completed_child_summaries=completed,
        failed_child_summaries=failed,
        dependency_blocked_summaries=blocked,
    )

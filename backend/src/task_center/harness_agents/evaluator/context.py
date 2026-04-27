"""Evaluator dispatch context construction."""

from __future__ import annotations

from dataclasses import dataclass, field

from task_center.graph.store import TaskGraph
from task_center.harness_agents.formatting import render_summaries
from task_center.model import HarnessGraphId, Task, TaskId, TaskSummary
from task_center.summaries import child_summary_groups

_EVALUATOR_PROMPT_INSTRUCTIONS = (
    "Read ROOT_GOAL, REQUEST_PLAN_NOTE, PLAN_HANDOFF_NOTE, and child summaries "
    "as context. Complete TASK_INPUT, which is the planner's evaluator_note, "
    "by verifying whether REQUEST_PLAN_NOTE was satisfied."
)


@dataclass
class EvaluatorLaunchContext:
    """Structural context for an evaluator task at dispatch time.

    All four notes (``root_goal``, ``request_plan_note``, ``handoff_plan_note``,
    ``evaluator_note``) are pulled from the harness graph. Child summaries
    are split by terminal kind so the evaluator can reason about each
    bucket separately.
    """

    task_id: TaskId
    harness_graph_id: HarnessGraphId
    root_goal: str
    request_plan_note: str
    handoff_plan_note: str
    evaluator_note: str
    success_child_summaries: list[TaskSummary] = field(default_factory=list)
    fail_child_summaries: list[TaskSummary] = field(default_factory=list)
    blocked_child_summaries: list[TaskSummary] = field(default_factory=list)

    def to_evaluator_prompt(self) -> str:
        return "\n\n".join(
            [
                f"## INSTRUCTIONS\n{_EVALUATOR_PROMPT_INSTRUCTIONS}",
                f"## ROOT_GOAL\n{self.root_goal}",
                f"## REQUEST_PLAN_NOTE\n{self.request_plan_note}",
                f"## PLAN_HANDOFF_NOTE\n{self.handoff_plan_note}",
                "## SUCCESS_CHILD_SUMMARIES\n"
                f"{render_summaries(self.success_child_summaries)}",
                "## FAIL_CHILD_SUMMARIES\n"
                f"{render_summaries(self.fail_child_summaries)}",
                "## BLOCKED_CHILD_SUMMARIES\n"
                f"{render_summaries(self.blocked_child_summaries)}",
                f"## TASK_INPUT\n{self.evaluator_note}",
            ]
        )


def build_evaluator_launch_context(
    graph: TaskGraph, task: Task
) -> EvaluatorLaunchContext | None:
    """Bundle the harness graph's notes and child summaries for an evaluator."""
    if task.role != "evaluator":
        raise ValueError("build_evaluator_launch_context requires an evaluator caller")
    if task.task_center_harness_graph_id is None:
        return None
    harness = graph.harness_graphs.get(task.task_center_harness_graph_id)
    if harness is None:
        return None
    success: list[TaskSummary] = []
    fail: list[TaskSummary] = []
    blocked: list[TaskSummary] = []
    for tid in harness.executor_task_ids:
        child = graph.tasks.get(tid)
        if child is None:
            continue
        child_completed, child_failed, child_blocked = child_summary_groups(child)
        success.extend(child_completed)
        fail.extend(child_failed)
        blocked.extend(child_blocked)
    return EvaluatorLaunchContext(
        task_id=task.id,
        harness_graph_id=task.task_center_harness_graph_id,
        root_goal=harness.root_goal,
        request_plan_note=harness.request_plan_note,
        handoff_plan_note=harness.handoff_plan_note,
        evaluator_note=harness.evaluator_note,
        success_child_summaries=success,
        fail_child_summaries=fail,
        blocked_child_summaries=blocked,
    )

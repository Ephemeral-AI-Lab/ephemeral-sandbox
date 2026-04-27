"""HarnessGraph — the planner/executor/evaluator decomposition unit."""

from __future__ import annotations

from dataclasses import dataclass, field

from task_center.model.task import HarnessGraphId, TaskId


@dataclass
class HarnessGraph:
    """One planner-led decomposition: planner + executor children + evaluator.

    The graph's ``root_task_id`` points at the executor or evaluator that
    launched the planner via ``request_plan``. The root executor is not
    inside any harness graph.

    The four note fields anchor every prompt rendered for this graph:

    - ``root_goal`` — the input of the immediate caller (the parent task)
      that invoked ``request_plan``. Captured once at graph creation.
    - ``request_plan_note`` — the verbatim ``request_plan_note`` argument
      the caller passed when invoking ``request_plan``. Captured once.
    - ``handoff_plan_note`` — the planner's ``handoff_plan_note`` from
      ``submit_plan_handoff`` (plan shape, topology, coverage map, GAP).
    - ``evaluator_note`` — the planner's explicit instruction to the
      evaluator from ``submit_plan_handoff`` (what to verify, what to
      skip, which adversarial probes are most relevant). Stored as the
      evaluator task's input.
    """

    id: HarnessGraphId
    run_id: str
    root_task_id: TaskId
    planner_task_id: TaskId
    root_goal: str = ""
    request_plan_note: str = ""
    handoff_plan_note: str = ""
    evaluator_note: str = ""
    evaluator_task_id: TaskId | None = None
    executor_task_ids: list[TaskId] = field(default_factory=list)

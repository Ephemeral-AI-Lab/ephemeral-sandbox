"""Executor dispatch context construction."""

from __future__ import annotations

from dataclasses import dataclass, field

from task_center.graph.store import TaskGraph
from task_center.model import HarnessGraphId, Status, Task, TaskId, TaskSummary

_EXECUTOR_PROMPT_INSTRUCTIONS = (
    "Read DEPENDENCY_SUMMARIES as locked-in context, then complete the work "
    "described in TASK_INPUT. TASK_INPUT is the task you own."
)


@dataclass
class DependencyBundle:
    """One DONE dependency's input and summaries, packaged for an executor."""

    task_id: TaskId
    task_input: str
    summaries: list[TaskSummary]


@dataclass
class ExecutorLaunchContext:
    """Structural context for an executor task at dispatch time."""

    task_id: TaskId
    task_input: str
    harness_graph_id: HarnessGraphId | None
    completed_dependencies: list[DependencyBundle] = field(default_factory=list)

    def to_executor_prompt(self) -> str:
        if not self.completed_dependencies:
            deps_block = "(none)"
        else:
            parts: list[str] = []
            for dep in self.completed_dependencies:
                summary_lines = "\n".join(
                    f"  - [{s.kind}] {s.text}" for s in dep.summaries
                ) or "  (no summaries)"
                parts.append(
                    f"### {dep.task_id}\n"
                    f"input: {dep.task_input}\n"
                    f"summaries:\n{summary_lines}"
                )
            deps_block = "\n\n".join(parts)
        return (
            f"## INSTRUCTIONS\n{_EXECUTOR_PROMPT_INSTRUCTIONS}\n\n"
            f"## TASK_INPUT\n{self.task_input}\n\n"
            f"## DEPENDENCY_SUMMARIES\n{deps_block}"
        )


def build_executor_launch_context(
    graph: TaskGraph, task: Task
) -> ExecutorLaunchContext:
    """Bundle an executor's task input with its DONE dependency summaries."""
    if task.role != "executor":
        raise ValueError("build_executor_launch_context requires an executor caller")
    deps: list[DependencyBundle] = []
    for dep_id in sorted(task.needs):
        dep = graph.tasks.get(dep_id)
        if dep is None or dep.status is not Status.DONE:
            continue
        deps.append(
            DependencyBundle(
                task_id=dep.id,
                task_input=dep.input,
                summaries=list(dep.summaries),
            )
        )
    return ExecutorLaunchContext(
        task_id=task.id,
        task_input=task.input,
        harness_graph_id=task.task_center_harness_graph_id,
        completed_dependencies=deps,
    )

"""Build the prompt sent to an agent for one TaskCenter task."""

from __future__ import annotations

import json
from typing import Any

from task_center.graph import TaskGraph
from task_center.task import Status, Task


def build_task_prompt(task: Task, graph: TaskGraph) -> str:
    """Return the user/task prompt with stable TaskCenter context injected.

    The task's own ``spec`` remains the task prompt. For child/evaluator tasks,
    prepend only the blackboard context they are allowed to see: parent
    acceptance information and completed direct dependencies.
    """
    context = _build_context_payload(task, graph)
    if context is None:
        return task.spec
    context_json = json.dumps(context, ensure_ascii=False, indent=2)
    return f"<Task Context>\n{context_json}\n</Task Context>\n\n<Task Prompt>\n{task.spec}\n</Task Prompt>"


def _build_context_payload(task: Task, graph: TaskGraph) -> dict[str, Any] | None:
    parent = graph.tasks.get(task.parent_id) if task.parent_id is not None else None
    dependencies = _completed_dependencies(task, graph)
    if parent is None and not dependencies:
        return None

    return {
        "parent": _parent_payload(parent) if parent is not None else None,
        "dependencies": dependencies,
    }


def _parent_payload(parent: Task) -> dict[str, str | None]:
    return {
        "id": parent.id,
        "acceptance_criteria": parent.acceptance_criteria,
        "handoff_note": parent.handoff_note,
    }


def _completed_dependencies(task: Task, graph: TaskGraph) -> list[dict[str, str | None]]:
    dependencies: list[dict[str, str | None]] = []
    for dep_id in sorted(task.needs):
        dep = graph.tasks.get(dep_id)
        if dep is None or dep.status is not Status.DONE:
            continue
        dependencies.append(
            {
                "id": dep.id,
                "spec": dep.spec,
                "summary": dep.summary,
            }
        )
    return dependencies

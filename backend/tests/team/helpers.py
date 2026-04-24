from __future__ import annotations

from team.core.models import Task, TaskDefinition, TaskStatus


def make_task(
    task_id: str,
    *,
    agent_name: str = "developer",
    status: TaskStatus = TaskStatus.PENDING,
    parent_id: str | None = "parent",
    deps: list[str] | None = None,
    fired_by_task_id: str | None = None,
) -> Task:
    return Task(
        id=task_id,
        team_run_id="run-1",
        definition=TaskDefinition(
            id=task_id,
            spec=structured_spec(f"task {task_id}"),
            agent=agent_name,
            deps=deps or [],
        ),
        status=status,
        parent_id=parent_id,
        root_id="root",
        depth=1 if parent_id else 0,
        fired_by_task_id=fired_by_task_id,
    )


def structured_spec(
    goal: str = "Complete the assigned task.",
    *,
    task_details: str | None = None,
    environment: str | None = None,
    scope: str | None = None,
    context: str | None = None,
    acceptance: str = "Submit the appropriate terminal outcome.",
) -> dict[str, str]:
    if task_details is None:
        detail_parts = [
            environment or "Use the current repository workspace and configured team runtime.",
            scope or "Stay within the listed scope_paths.",
        ]
        if context:
            detail_parts.append(context)
        task_details = " ".join(detail_parts)
    return {
        "goal": goal,
        "detail": task_details,
        "acceptance_criteria": acceptance,
    }

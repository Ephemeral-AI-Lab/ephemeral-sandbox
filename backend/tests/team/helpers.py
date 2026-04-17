from __future__ import annotations

from team.models import Task, TaskStatus


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
        agent_name=agent_name,
        status=status,
        objective=f"task {task_id}",
        deps=deps or [],
        parent_id=parent_id,
        root_id="root",
        depth=1 if parent_id else 0,
        fired_by_task_id=fired_by_task_id,
    )


def structured_spec(
    goal: str = "Complete the assigned task.",
    *,
    environment: str = "Use the current repository workspace and configured team runtime.",
    scope: str = "Stay within the listed scope_paths.",
    context: str = "Created by the team runtime.",
    acceptance: str = "Submit the appropriate terminal outcome.",
) -> str:
    return (
        f"1. Goal: {goal}\n"
        f"2. Environment: {environment}\n"
        f"3. Scope: {scope}\n"
        f"4. Context: {context}\n"
        f"5. Acceptance Criteria: {acceptance}"
    )

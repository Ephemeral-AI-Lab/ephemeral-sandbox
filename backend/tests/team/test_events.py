from __future__ import annotations

from datetime import datetime, timezone

import pytest

from team.models import Task, TaskStatus
from team.persistence.events import task_from_dict, task_to_dict


def _task() -> Task:
    return Task(
        id="task-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.RUNNING,
        objective="repair shared import",
        deps=["dep-1"],
        scope_paths=["pkg/_compat.py"],
        agent_run_id="agent-run-1",
        created_at=datetime(2026, 4, 14, tzinfo=timezone.utc),
    )


def test_task_serialization_round_trip_preserves_task_fields():
    original = _task()
    original.description = "Repair shared import label"

    payload = task_to_dict(original)
    restored = task_from_dict(payload)

    assert restored.description == "Repair shared import label"
    assert restored.deps == ["dep-1"]
    assert restored.scope_paths == ["pkg/_compat.py"]
    assert restored.agent_run_id == "agent-run-1"


def test_task_from_dict_requires_objective():
    with pytest.raises(ValueError, match="Task payload requires a non-empty 'objective'"):
        task_from_dict(
            {
                "id": "task-1",
                "team_run_id": "run-1",
                "agent_name": "developer",
                "status": "pending",
                "task": "repair shared import",
            }
        )

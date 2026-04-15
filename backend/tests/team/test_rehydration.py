from __future__ import annotations

from datetime import datetime, timezone
from types import SimpleNamespace

import pytest

from team.models import Task, TaskStatus
from team.persistence.events import make_task_status, task_to_dict
from team.runtime.rehydration import apply_replayed_event, task_from_dict


def _task() -> Task:
    return Task(
        id="task-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.PAUSED,
        objective="repair shared import",
        deps=["dep-1"],
        scope_paths=["pkg/_compat.py"],
        pending_dep_count=1,
        retry_count=2,
        max_retries=4,
        agent_run_id="agent-run-1",
        created_at=datetime(2026, 4, 14, tzinfo=timezone.utc),
        blocker_id="blocker-1",
        pause_checkpoint='[{"role":"assistant","content":"paused"}]',
        pause_verdict="Shared import break requires pause.",
    )


def test_task_serialization_round_trip_preserves_blocker_pause_fields():
    original = _task()

    payload = task_to_dict(original)
    restored = task_from_dict(payload)

    assert restored.pending_dep_count == 1
    assert restored.blocker_id == "blocker-1"
    assert restored.pause_checkpoint == '[{"role":"assistant","content":"paused"}]'
    assert restored.pause_verdict == "Shared import break requires pause."


def test_task_from_dict_rejects_legacy_task_field():
    with pytest.raises(ValueError, match="Task payload uses legacy 'task'; use 'objective'"):
        task_from_dict(
            {
                "id": "task-1",
                "team_run_id": "run-1",
                "agent_name": "developer",
                "status": "pending",
                "task": "repair shared import",
            }
        )


def test_apply_replayed_event_updates_blocker_pause_fields():
    task = _task()
    task.status = TaskStatus.RUNNING
    task.blocker_id = None
    task.pause_checkpoint = None
    task.pause_verdict = None
    graph = {task.id: task}

    event = make_task_status(
        "run-1",
        task.id,
        TaskStatus.PAUSED.value,
        blocker_id="blocker-1",
        pause_checkpoint='[{"role":"assistant","content":"paused"}]',
        pause_verdict="Shared import break requires pause.",
    )

    root_id, budget, final_status = apply_replayed_event(
        event=event,
        graph=graph,
        services=SimpleNamespace(),
        root_id=None,
    )

    assert root_id is None
    assert budget is None
    assert final_status is None
    assert graph[task.id].status == TaskStatus.PAUSED
    assert graph[task.id].blocker_id == "blocker-1"
    assert graph[task.id].pause_checkpoint == '[{"role":"assistant","content":"paused"}]'
    assert graph[task.id].pause_verdict == "Shared import break requires pause."


def test_apply_replayed_event_keeps_existing_status_when_event_status_is_unknown():
    task = _task()
    graph = {task.id: task}
    event = SimpleNamespace(kind="task_status", data={"task_id": task.id, "status": "mystery"})

    root_id, budget, final_status = apply_replayed_event(
        event=event,
        graph=graph,
        services=SimpleNamespace(),
        root_id=None,
    )

    assert root_id is None
    assert budget is None
    assert final_status is None
    assert graph[task.id].status == TaskStatus.PAUSED

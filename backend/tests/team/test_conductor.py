from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from team.models import Blocker, BlockerStatus, TeamRunStatus
from team.runtime.conductor import Conductor


class _FakeBlockerStore:
    def __init__(self) -> None:
        self.saved: list[Blocker] = []

    async def save(self, blocker: Blocker) -> None:
        self.saved.append(blocker)


@pytest.mark.asyncio
async def test_on_fix_failed_fails_run_and_clears_active_blocker():
    blocker_store = _FakeBlockerStore()
    task_center = SimpleNamespace(cancel_paused_tasks=AsyncMock(return_value=2))
    team_run = SimpleNamespace(
        id="run-1",
        task_center=task_center,
        status=TeamRunStatus.RUNNING,
        fail_due_to_blocker=AsyncMock(),
    )
    conductor = Conductor(team_run, blocker_store=blocker_store)
    blocker = Blocker(
        id="blocker-1",
        team_run_id="run-1",
        status=BlockerStatus.FIXING,
        reason="shared import crash",
        root_cause_paths=["pkg/_compat.py"],
        initiating_task_id="task-1",
        fix_task_id="resolver-1",
    )
    conductor._active_blockers[blocker.id] = blocker

    await conductor.on_fix_failed(blocker.id, "resolver exhausted repair options")

    task_center.cancel_paused_tasks.assert_awaited_once_with(blocker.id)
    team_run.fail_due_to_blocker.assert_awaited_once()
    assert blocker_store.saved[-1].status == BlockerStatus.FAILED
    assert conductor.has_active_blocker() is False


@pytest.mark.asyncio
async def test_on_fix_failed_falls_back_to_status_when_team_run_has_no_handler():
    blocker_store = _FakeBlockerStore()
    task_center = SimpleNamespace(cancel_paused_tasks=AsyncMock(return_value=1))
    team_run = SimpleNamespace(
        id="run-1",
        task_center=task_center,
        status=TeamRunStatus.RUNNING,
    )
    conductor = Conductor(team_run, blocker_store=blocker_store)
    blocker = Blocker(
        id="blocker-1",
        team_run_id="run-1",
        status=BlockerStatus.FIXING,
        reason="shared import crash",
        root_cause_paths=["pkg/_compat.py"],
        initiating_task_id="task-1",
    )
    conductor._active_blockers[blocker.id] = blocker

    await conductor.on_fix_failed(blocker.id, "no viable repair")

    assert team_run.status == TeamRunStatus.FAILED
    assert blocker_store.saved[-1].status == BlockerStatus.FAILED

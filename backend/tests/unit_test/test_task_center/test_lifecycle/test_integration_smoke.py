"""Manager/handler closure smoke with a synchronous attempt closer."""

from __future__ import annotations

from collections.abc import Callable

from db.stores.attempt_store import AttemptStore
from task_center.config import TaskCenterLifecycleConfig
from task_center.mission.handler import MissionHandler
from task_center.episode.manager import EpisodeManager
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.mission.mission import MissionStatus
from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStatus,
)
from task_center.episode.episode import EpisodeStatus


class _StubOrchestrator:
    """Synchronous stand-in for AttemptOrchestrator.

    Closes the attempt immediately on ``start`` with a caller-supplied verdict.
    """

    def __init__(
        self,
        *,
        attempt: Attempt,
        attempt_store: AttemptStore,
        on_attempt_closed: Callable[[str], None],
        verdict: tuple[
            AttemptStatus, AttemptFailReason | None, str | None
        ],
    ) -> None:
        self._g = attempt
        self._gs = attempt_store
        self._cb = on_attempt_closed
        self._verdict = verdict

    def start(self) -> None:
        status, fail_reason, continuation_goal = self._verdict
        if continuation_goal is not None:
            self._gs.set_plan_contract(
                self._g.id,
                task_specification="stub-spec",
                evaluation_criteria=["stub-criterion"],
                continuation_goal=continuation_goal,
            )
        self._gs.close(self._g.id, status=status, fail_reason=fail_reason)
        self._cb(self._g.id)


def _build_handler(mission_store, episode_store, attempt_store):
    return MissionHandler(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        manager_registry=EpisodeManagerRegistry(),
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
    )


def _drive_segment(
    *,
    handler,
    episode_id: str,
    attempt_store: AttemptStore,
    verdict: tuple[
        AttemptStatus, AttemptFailReason | None, str | None
    ],
) -> None:
    """Run a stub orchestrator against the manager-owned episode."""
    registry = handler._manager_registry  # type: ignore[attr-defined]
    mgr: EpisodeManager | None = registry.get(episode_id)
    assert mgr is not None
    g = mgr.create_initial_attempt()
    stub = _StubOrchestrator(
        attempt=g,
        attempt_store=attempt_store,
        on_attempt_closed=mgr.handle_attempt_closed,
        verdict=verdict,
    )
    stub.start()


def test_smoke_terminal_success(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    handler = _build_handler(mission_store, episode_store, attempt_store)
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="exec-1",
        goal="solve X",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    _drive_segment(
        handler=handler,
        episode_id=seg.id,
        attempt_store=attempt_store,
        verdict=(AttemptStatus.PASSED, None, None),
    )
    final_request = mission_store.get(req.id)
    final_segment = episode_store.get(seg.id)
    assert final_request is not None and final_segment is not None
    assert final_request.status == MissionStatus.SUCCEEDED
    assert final_segment.status == EpisodeStatus.SUCCEEDED


def test_smoke_attempt_plan_failed(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    handler = _build_handler(mission_store, episode_store, attempt_store)
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="exec-1",
        goal="solve X",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    # First attempt: fail with a generator error.
    registry = handler._manager_registry  # type: ignore[attr-defined]
    mgr = registry.get(seg.id)
    assert mgr is not None
    g1 = mgr.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id, task_specification="spec1", evaluation_criteria=["a"], continuation_goal=None
    )
    attempt_store.close(
        g1.id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    mgr.handle_attempt_closed(g1.id)
    # Second (and budget-final) attempt: also fail.
    seg_after = episode_store.get(seg.id)
    assert seg_after is not None
    g2_id = seg_after.attempt_ids[-1]
    attempt_store.set_plan_contract(
        g2_id, task_specification="spec2", evaluation_criteria=["b"], continuation_goal=None
    )
    attempt_store.close(
        g2_id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    mgr.handle_attempt_closed(g2_id)
    final_request = mission_store.get(req.id)
    final_segment = episode_store.get(seg.id)
    assert final_request is not None and final_segment is not None
    assert final_request.status == MissionStatus.FAILED
    assert final_segment.status == EpisodeStatus.FAILED
    assert final_request.final_outcome is not None
    assert final_request.final_outcome["outcome"] == "failed"


def test_smoke_success_continue_then_terminal(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    handler = _build_handler(mission_store, episode_store, attempt_store)
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="exec-1",
        goal="initial-goal",
    )
    seg1, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    _drive_segment(
        handler=handler,
        episode_id=seg1.id,
        attempt_store=attempt_store,
        verdict=(AttemptStatus.PASSED, None, "next-goal"),
    )
    refreshed = mission_store.get(req.id)
    assert refreshed is not None
    assert len(refreshed.episode_ids) == 2
    assert refreshed.is_open
    seg2_id = refreshed.episode_ids[1]
    seg2 = episode_store.get(seg2_id)
    assert seg2 is not None
    assert seg2.goal == "next-goal"
    # Drive episode 2 to terminal success.
    _drive_segment(
        handler=handler,
        episode_id=seg2_id,
        attempt_store=attempt_store,
        verdict=(AttemptStatus.PASSED, None, None),
    )
    final_request = mission_store.get(req.id)
    assert final_request is not None
    assert final_request.status == MissionStatus.SUCCEEDED

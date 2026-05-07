"""Canonical ancestor walks across the mission → episode → attempt → task chain.

This module owns the single implementation of the partial-plan ancestor
predicate used by agent-launch predicate resolution.
"""

from __future__ import annotations

from db.stores.mission_store import MissionStore
from db.stores.attempt_store import AttemptStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.episode_store import EpisodeStore
from task_center.exceptions import TaskCenterInvariantViolation


def has_partial_planned_caller_ancestor(
    *,
    mission_id: str,
    mission_store: MissionStore,
    episode_store: EpisodeStore,
    attempt_store: AttemptStore,
    task_store: TaskCenterStore,
) -> bool:
    """Return True iff any caller attempt in the ancestry submitted a partial plan.

    Walks ``parent_task → parent_attempt → parent_episode → parent_mission``
    upward from ``mission_id`` until a partial-planned caller is found
    (``parent_attempt.continuation_goal`` is non-null) or the chain terminates
    (top-level entry executor — no caller attempt).

    Raises :class:`TaskCenterInvariantViolation` on cycles and on missing
    intermediate rows once the chain has begun. A missing parent task or a
    parent task with no ``task_center_attempt_id`` terminates the walk
    cleanly (top-level case).
    """
    seen_mission_ids: set[str] = set()
    current_mission_id = mission_id

    while True:
        if current_mission_id in seen_mission_ids:
            raise TaskCenterInvariantViolation(
                "Cycle detected while resolving mission ancestry."
            )
        seen_mission_ids.add(current_mission_id)

        current_mission = mission_store.get(current_mission_id)
        if current_mission is None:
            raise TaskCenterInvariantViolation(
                f"Mission {current_mission_id!r} was not found."
            )

        parent_task = task_store.get_task(current_mission.requested_by_task_id)
        if parent_task is None:
            return False

        parent_attempt_id = str(parent_task.get("task_center_attempt_id") or "")
        if not parent_attempt_id:
            return False

        parent_attempt = attempt_store.get(parent_attempt_id)
        if parent_attempt is None:
            raise TaskCenterInvariantViolation(
                f"Parent Attempt {parent_attempt_id!r} was not found."
            )

        if parent_attempt.continuation_goal is not None:
            return True

        parent_episode = episode_store.get(parent_attempt.episode_id)
        if parent_episode is None:
            raise TaskCenterInvariantViolation(
                f"Parent Episode {parent_attempt.episode_id!r} was not found."
            )

        current_mission_id = parent_episode.mission_id

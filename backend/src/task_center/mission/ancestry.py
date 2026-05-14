"""Canonical ancestor walks across the mission → episode → attempt → task chain.

This module owns the depth helper used by agent-launch predicate resolution.
``nested_mission_depth`` counts how many missions appear on the ancestry chain
starting from a given mission id, inclusive of that mission. Predicates gate
on the depth via the ``MAX_HANDOFF_DEPTH`` constant rather than walking the
chain themselves.
"""

from __future__ import annotations

from task_center.persistence import MissionStoreProtocol
from task_center.persistence import AttemptStoreProtocol
from task_center.persistence import TaskStoreProtocol
from task_center.persistence import EpisodeStoreProtocol
from task_center.exceptions import TaskCenterInvariantViolation


def nested_mission_depth(
    *,
    mission_id: str,
    mission_store: MissionStoreProtocol,
    episode_store: EpisodeStoreProtocol,
    attempt_store: AttemptStoreProtocol,
    task_store: TaskStoreProtocol,
) -> int:
    """Return the number of mission ancestors on the chain INCLUDING ``mission_id``.

    Walks ``parent_task → parent_attempt → parent_episode → parent_mission``
    upward from ``mission_id`` until the chain terminates (top-level entry
    executor — no caller attempt). The mission itself counts as depth 1, its
    immediate parent mission as depth 2, and so on.

    Callers MUST pass a non-``None`` ``mission_id``; the entry-executor scope
    (``mission_id is None``) is handled by the resolver before this function
    is invoked.

    Raises :class:`TaskCenterInvariantViolation` on cycles and on missing
    intermediate rows once the chain has begun. A missing parent task or a
    parent task with no ``task_center_attempt_id`` terminates the walk
    cleanly (top-level case).
    """
    depth = 0
    seen_mission_ids: set[str] = set()
    current_mission_id = mission_id

    while True:
        if current_mission_id in seen_mission_ids:
            raise TaskCenterInvariantViolation(
                "Cycle detected while resolving mission ancestry."
            )
        seen_mission_ids.add(current_mission_id)
        depth += 1

        current_mission = mission_store.get(current_mission_id)
        if current_mission is None:
            raise TaskCenterInvariantViolation(
                f"Mission {current_mission_id!r} was not found."
            )

        parent_task = task_store.get_task(current_mission.requested_by_task_id)
        if parent_task is None:
            return depth

        parent_attempt_id = str(parent_task.get("task_center_attempt_id") or "")
        if not parent_attempt_id:
            return depth

        parent_attempt = attempt_store.get(parent_attempt_id)
        if parent_attempt is None:
            raise TaskCenterInvariantViolation(
                f"Parent Attempt {parent_attempt_id!r} was not found."
            )

        parent_episode = episode_store.get(parent_attempt.episode_id)
        if parent_episode is None:
            raise TaskCenterInvariantViolation(
                f"Parent Episode {parent_attempt.episode_id!r} was not found."
            )

        current_mission_id = parent_episode.mission_id

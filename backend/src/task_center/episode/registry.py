"""Process-local registry: one :class:`EpisodeManager` per open ``Episode``."""

from __future__ import annotations

from task_center.episode.manager import EpisodeManager
from task_center._core.types import TaskCenterInvariantViolation


class EpisodeManagerRegistry:
    """In-memory registry enforcing one-manager-per-open-episode."""

    def __init__(self) -> None:
        self._by_episode_id: dict[str, EpisodeManager] = {}

    def register(self, manager: EpisodeManager) -> None:
        episode_id = manager.episode_id
        if episode_id in self._by_episode_id:
            raise TaskCenterInvariantViolation(
                f"EpisodeManager already registered for episode {episode_id!r}"
            )
        self._by_episode_id[episode_id] = manager

    def get(self, episode_id: str) -> EpisodeManager | None:
        return self._by_episode_id.get(episode_id)

    def deregister(self, episode_id: str) -> None:
        self._by_episode_id.pop(episode_id, None)

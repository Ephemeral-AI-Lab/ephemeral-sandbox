"""ContextEngine — single ``build(recipe_id, scope)`` entry point.

The engine owns no role names. Every recipe is registered against a string id
and looked up at call time. Recipes receive :class:`ContextScope` and a
shared :class:`ContextEngineDeps` bundle.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from task_center.context_engine.packet import ContextPacket
from task_center.context_engine.recipes_registry import RecipeRegistry
from task_center.context_engine.scope import ContextScope

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from task_center._core.persistence import MissionStoreProtocol
    from task_center._core.persistence import AttemptStoreProtocol
    from task_center._core.persistence import TaskStoreProtocol
    from task_center._core.persistence import EpisodeStoreProtocol


class ContextPacketStoreProtocol(Protocol):
    def insert(self, packet: ContextPacket) -> str: ...

    def get(self, context_packet_id: str) -> ContextPacket | None: ...


@dataclass(frozen=True, slots=True)
class ContextEngineDeps:
    """Frozen bundle of stores recipes may read from.

    The bundle is intentionally narrow: recipes never reach for globals or
    runtime objects, so swapping a store in tests is one keyword argument.
    """

    mission_store: MissionStoreProtocol
    episode_store: EpisodeStoreProtocol
    attempt_store: AttemptStoreProtocol
    task_store: TaskStoreProtocol

    # Optional: when supplied, the composer persists rendered packet inputs.
    context_packet_store: ContextPacketStoreProtocol | None = None


class ContextEngine:
    """Routes recipe ids to registered builders."""

    def __init__(
        self,
        deps: ContextEngineDeps,
    ) -> None:
        self._deps = deps

    @property
    def deps(self) -> ContextEngineDeps:
        return self._deps

    def build(self, recipe_id: str, scope: ContextScope) -> ContextPacket:
        recipe = RecipeRegistry.get(recipe_id)
        scope.assert_fields(recipe.required_scope_fields)
        return recipe.build(scope, self._deps)

"""ContextEngine — single ``build(recipe_id, scope)`` entry point.

The engine owns no role names. Every recipe is registered against a string id
and looked up at call time. Recipes receive :class:`ContextScope` and a
shared :class:`ContextEngineDeps` bundle.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from task_center.context_engine.packet import ContextPacket
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)
from task_center.context_engine.scope import ContextScope

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from db.stores.complex_task_request_store import ComplexTaskRequestStore
    from db.stores.harness_graph_store import HarnessGraphStore
    from db.stores.task_center_store import TaskCenterStore
    from db.stores.task_segment_store import TaskSegmentStore


@dataclass(frozen=True, slots=True)
class ContextEngineDeps:
    """Frozen bundle of stores recipes may read from.

    The bundle is intentionally narrow: recipes never reach for globals or
    runtime objects, so swapping a store in tests is one keyword argument.
    """

    request_store: "ComplexTaskRequestStore"
    segment_store: "TaskSegmentStore"
    graph_store: "HarnessGraphStore"
    task_store: "TaskCenterStore"

    # Helper recipes (advisor / resolver) load parent packets from this store.
    # Optional so non-helper recipes can be tested without spinning one up.
    context_packet_store: object | None = None


class ContextEngine:
    """Routes recipe ids to registered builders."""

    def __init__(
        self,
        deps: ContextEngineDeps,
        *,
        registry: type[RecipeRegistry] = RecipeRegistry,
    ) -> None:
        self._deps = deps
        self._registry = registry

    @property
    def deps(self) -> ContextEngineDeps:
        return self._deps

    def build(self, recipe_id: str, scope: ContextScope) -> ContextPacket:
        recipe = self._registry.get(recipe_id)
        scope.assert_fields(recipe.required_scope_fields)
        return recipe.build(scope, self._deps)

    def get_recipe(self, recipe_id: str) -> ContextRecipe:
        return self._registry.get(recipe_id)

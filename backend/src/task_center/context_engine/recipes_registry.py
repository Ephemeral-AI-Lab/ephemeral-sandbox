"""Recipe registry — dispatches :class:`ContextRecipe` builders by id.

The registry is a *process-global* singleton. Tests should call
:meth:`RecipeRegistry.clear` in their teardown when registering ad-hoc
recipes; production startup calls
:func:`task_center.context_engine.recipes.register_builtin_recipes` exactly
once.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import ContextPacket
from task_center.context_engine.scope import ContextScope

# Forward references avoid circular imports — the engine imports recipes_registry,
# recipes_registry stays free of engine-deps.
RecipeBuild = Callable[[ContextScope, "ContextEngineDepsLike"], ContextPacket]  # type: ignore[name-defined]  # noqa: F821


@dataclass(frozen=True, slots=True)
class ContextRecipe:
    """One registered recipe."""

    id: str
    required_scope_fields: frozenset[str]
    build: RecipeBuild


class RecipeRegistry:
    """Process-global recipe registry."""

    _registry: dict[str, ContextRecipe] = {}

    @classmethod
    def register(cls, recipe: ContextRecipe) -> None:
        cls._registry[recipe.id] = recipe

    @classmethod
    def get(cls, recipe_id: str) -> ContextRecipe:
        try:
            return cls._registry[recipe_id]
        except KeyError as exc:
            raise ContextEngineError(
                f"Recipe {recipe_id!r} is not registered. "
                f"Known recipes: {sorted(cls._registry)!r}"
            ) from exc

    @classmethod
    def has(cls, recipe_id: str) -> bool:
        return recipe_id in cls._registry

    @classmethod
    def list_ids(cls) -> list[str]:
        return sorted(cls._registry)

    @classmethod
    def clear(cls) -> None:
        cls._registry.clear()

"""Built-in context recipes.

Adding a new recipe is two steps: write the builder in its own module under
this package, then call :func:`register_builtin_recipes` (idempotent) at
startup. The engine itself owns no recipe knowledge.
"""

from __future__ import annotations

from task_center.context_engine.recipes.entry_executor import (
    ENTRY_EXECUTOR_V1,
    ENTRY_EXECUTOR_V1_RECIPE,
)
from task_center.context_engine.recipes.evaluator import (
    EVALUATOR_V1,
    EVALUATOR_V1_RECIPE,
)
from task_center.context_engine.recipes.generator import (
    GENERATOR_V1,
    GENERATOR_V1_RECIPE,
)
from task_center.context_engine.recipes.planner import (
    PLANNER_V1,
    PLANNER_V1_RECIPE,
)
from task_center.context_engine.recipes_registry import RecipeRegistry

_BUILTIN_RECIPES = (
    PLANNER_V1_RECIPE,
    GENERATOR_V1_RECIPE,
    EVALUATOR_V1_RECIPE,
    ENTRY_EXECUTOR_V1_RECIPE,
)


def register_builtin_recipes() -> None:
    """Register every built-in recipe. Idempotent — safe to call repeatedly."""
    for recipe in _BUILTIN_RECIPES:
        RecipeRegistry.register(recipe)


__all__ = [
    "ENTRY_EXECUTOR_V1",
    "EVALUATOR_V1",
    "GENERATOR_V1",
    "PLANNER_V1",
    "register_builtin_recipes",
]

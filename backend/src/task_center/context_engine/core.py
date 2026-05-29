"""Context engine — recipe-id-to-packet routing.

``ContextEngine`` looks up a registered :class:`ContextRecipe` by id and runs
it against the caller's :class:`ContextScope`, producing a
:class:`ContextPacket`. Composer + launch wiring live in
:mod:`task_center.agent_launch`.

Exceptions are re-exported from :mod:`.exceptions` so existing callers that
``from task_center.context_engine.core import ContextEngineError`` keep
working.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from task_center.context_engine.exceptions import (
    AgentDefinitionValidationError,
    ContextEngineError,
    MissingContextRecipeError,
    RecipeScopeError,
)
from task_center.context_engine.packet import ContextPacket
from task_center.context_engine.recipes_registry import RecipeRegistry
from task_center.context_engine.scope import ContextScope

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from task_center._core.persistence import (
        AttemptStoreProtocol,
        IterationStoreProtocol,
        WorkflowStoreProtocol,
        TaskStoreProtocol,
    )

__all__ = [
    "AgentDefinitionValidationError",
    "ContextEngine",
    "ContextEngineDeps",
    "ContextEngineError",
    "ContextPacketStoreProtocol",
    "MissingContextRecipeError",
    "RecipeScopeError",
]


class ContextPacketStoreProtocol(Protocol):
    def insert(self, packet: ContextPacket) -> str: ...


@dataclass(frozen=True, slots=True)
class ContextEngineDeps:
    """Frozen bundle of stores recipes may read from.

    Recipes never reach for globals or runtime objects, so swapping a store in
    tests is one keyword argument.
    """

    workflow_store: WorkflowStoreProtocol
    iteration_store: IterationStoreProtocol
    attempt_store: AttemptStoreProtocol
    task_store: TaskStoreProtocol

    # Optional: when supplied, the composer persists rendered packet inputs.
    context_packet_store: ContextPacketStoreProtocol | None = None


@dataclass(frozen=True, slots=True)
class ContextEngine:
    """Routes recipe ids to registered builders."""

    deps: ContextEngineDeps

    def build(self, recipe_id: str, scope: ContextScope) -> ContextPacket:
        recipe = RecipeRegistry.get(recipe_id)
        scope.assert_fields(recipe.required_scope_fields)
        return recipe.build(scope, self.deps)

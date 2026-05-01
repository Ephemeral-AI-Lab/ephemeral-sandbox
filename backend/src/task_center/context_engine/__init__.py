"""Context engine — flexible composition of agent / system / user prompts.

Public surface (per plan §3.1):

* :class:`ContextPacket`, :class:`ContextBlock` — typed packet schema.
* :class:`ContextScope` — the discriminated-union surface every recipe sees.
* :class:`ContextRecipe`, :class:`RecipeRegistry` — recipe dispatch.
* :class:`ContextEngine`, :class:`ContextEngineDeps` — packet builder.
* :class:`PromptRenderer`, :class:`MarkdownPromptRenderer` — pure renderer.
* :class:`AgentResolver`, :class:`AgentSelection`,
  :class:`RuleBasedAgentResolver` — frontmatter-driven variant selection.
* :class:`ContextComposer`, :class:`LaunchBundle` — single launch entry point.
* :class:`PredicateRegistry`, :class:`ResolverContext` — named predicates.
"""

from __future__ import annotations

from task_center.context_engine.errors import (
    ContextEngineError,
    MissingContextRecipeError,
    RecipeScopeError,
)
from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.scope import ContextScope

__all__ = [
    "ContextBlock",
    "ContextBlockKind",
    "ContextEngineError",
    "ContextPacket",
    "ContextPriority",
    "ContextRefs",
    "ContextScope",
    "MissingContextRecipeError",
    "RecipeScopeError",
]

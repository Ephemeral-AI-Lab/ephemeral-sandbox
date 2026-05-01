"""Exceptions raised by the context engine.

Pulled into a single module so every other engine file imports from one place
and reverse-imports stay simple.
"""

from __future__ import annotations


class ContextEngineError(Exception):
    """Generic context engine failure (e.g. missing prior-segment fields)."""


class RecipeScopeError(ContextEngineError):
    """A recipe was called with a :class:`ContextScope` missing required fields."""


class MissingContextRecipeError(ContextEngineError):
    """An agent definition was selected for composition but has no
    ``context_recipe`` declared in frontmatter."""


class AgentDefinitionValidationError(ContextEngineError):
    """A registered :class:`AgentDefinition` references unknown or invalid
    variants / predicates / context recipes — caught at startup."""

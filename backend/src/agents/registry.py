"""Runtime registry for config-backed agent definitions."""

from __future__ import annotations

from typing import TYPE_CHECKING

from agents.types import AgentDefinition

if TYPE_CHECKING:  # pragma: no cover
    from task_center.context_engine.predicates import PredicateRegistry as _PR
    from task_center.context_engine.recipes_registry import (
        RecipeRegistry as _RR,
    )

# ---------------------------------------------------------------------------
# Builtin definitions
# ---------------------------------------------------------------------------

# No repository-bundled agent names are reserved by default.
RESERVED_BUILTIN_AGENT_NAMES: frozenset[str] = frozenset()


# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------

_DEFINITIONS: dict[str, AgentDefinition] = {}


def register_definition(defn: AgentDefinition) -> None:
    """Register or replace an agent definition at runtime."""
    _DEFINITIONS[defn.name] = defn


def unregister_definition(name: str) -> bool:
    """Remove an agent definition. Returns True if it existed."""
    return _DEFINITIONS.pop(name, None) is not None


def get_definition(name: str) -> AgentDefinition | None:
    """Look up an agent definition by name."""
    return _DEFINITIONS.get(name)


def list_definitions() -> list[AgentDefinition]:
    """List all registered definitions."""
    return list(_DEFINITIONS.values())


def get_role(agent_name: str) -> str | None:
    """Return the ``role`` tag for *agent_name*, or ``None``."""
    defn = get_definition(agent_name)
    return defn.role if defn is not None else None


def has_role(agent_name: str, role: str) -> bool:
    """Check whether *agent_name* is registered with the given *role*."""
    return get_role(agent_name) == role


def find_by_role(role: str) -> list[AgentDefinition]:
    """Return all registered definitions whose ``role`` matches."""
    return [d for d in _DEFINITIONS.values() if d.role == role]


def list_dispatchable_subagent_names() -> list[str]:
    """Return registered subagent names that may be targeted by run_subagent."""
    return sorted(
        defn.name
        for defn in _DEFINITIONS.values()
        if defn.agent_type == "subagent"
    )


# ---------------------------------------------------------------------------
# Startup validation: ensure every variants:/context_recipe: reference resolves
# ---------------------------------------------------------------------------


def validate_agent_definitions_resolved(
    *,
    predicate_registry: type["_PR"] | None = None,
    recipe_registry: type["_RR"] | None = None,
) -> None:
    """Cross-check every registered :class:`AgentDefinition`.

    Raises :class:`AgentDefinitionValidationError` if any agent references an
    unregistered predicate / recipe / variant target, or declares a variant
    target that itself has variants (chaining is forbidden).

    Called once at app startup after ``load_agents_tree`` so wiring mistakes
    surface before the first request.
    """
    # Imports kept local so agents/registry.py does not depend on the context
    # engine at module load.
    from task_center.context_engine.errors import (
        AgentDefinitionValidationError,
    )
    from task_center.context_engine.predicates import (
        PredicateRegistry as DefaultPredicateRegistry,
    )
    from task_center.context_engine.recipes_registry import (
        RecipeRegistry as DefaultRecipeRegistry,
    )

    predicates = predicate_registry or DefaultPredicateRegistry
    recipes = recipe_registry or DefaultRecipeRegistry

    for definition in _DEFINITIONS.values():
        _validate_definition(
            definition,
            predicates=predicates,
            recipes=recipes,
            error_cls=AgentDefinitionValidationError,
        )


def _validate_definition(
    definition: AgentDefinition,
    *,
    predicates: type["_PR"],
    recipes: type["_RR"],
    error_cls: type[Exception],
) -> None:
    if definition.context_recipe and not recipes.has(definition.context_recipe):
        raise error_cls(
            f"Agent {definition.name!r} declares context_recipe="
            f"{definition.context_recipe!r}, which is not registered."
        )
    for variant in definition.variants:
        if not predicates.has(variant.when):
            raise error_cls(
                f"Agent {definition.name!r} variant references unknown "
                f"predicate {variant.when!r}."
            )
        target = get_definition(variant.use)
        if target is None:
            raise error_cls(
                f"Agent {definition.name!r} variant points to unknown agent "
                f"{variant.use!r}."
            )
        if target.variants:
            raise error_cls(
                f"Agent {definition.name!r} variant target {target.name!r} "
                "declares its own variants — chaining is forbidden."
            )
        if target.context_recipe and not recipes.has(target.context_recipe):
            raise error_cls(
                f"Variant target {target.name!r} declares context_recipe="
                f"{target.context_recipe!r}, which is not registered."
            )

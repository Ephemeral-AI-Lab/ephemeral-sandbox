"""Resolved-reference validation for registered agent definitions."""

from __future__ import annotations

from .registry import get_definition, list_definitions
from .model import AgentDefinition


# Predicate id reserved for the total-coverage tail of a variants list. Lint
# rules below require this name to appear as the FINAL ``when`` entry in any
# variants-having profile that risks silent no-match — see plan AC9.
_TAIL_PREDICATE = "always"


def validate_agent_definitions_resolved() -> None:
    """Cross-check every registered :class:`AgentDefinition`.

    Raises :class:`AgentDefinitionValidationError` if any agent references an
    unregistered predicate / recipe / variant target, declares a variant
    target that itself has variants (chaining is forbidden), or violates the
    total-coverage tail rule on its variants list. Also runs the row-4
    terminal-silence lint over every declared skill file
    (:func:`agents.skills.validate_skill_files`).

    Called once at app startup after ``load_agents_tree`` so wiring mistakes
    surface before the first request.
    """
    definitions = list_definitions()
    for definition in definitions:
        _validate_definition(definition)

    # Skill-file lint runs after cross-reference validation so the failure
    # message points at a real, resolvable definition. Lazy import avoids a
    # registry-vs-loader import cycle.
    from agents.skills import validate_skill_files

    validate_skill_files(definitions)


def _validate_definition(definition: AgentDefinition) -> None:
    from task_center import (
        AgentDefinitionValidationError,
        PredicateRegistry,
        RecipeRegistry,
    )

    if definition.context_recipe and not RecipeRegistry.has(definition.context_recipe):
        raise AgentDefinitionValidationError(
            f"Agent {definition.name!r} declares context_recipe="
            f"{definition.context_recipe!r}, which is not registered."
        )
    for variant in definition.variants:
        if not PredicateRegistry.has(variant.when):
            raise AgentDefinitionValidationError(
                f"Agent {definition.name!r} variant references unknown "
                f"predicate {variant.when!r}."
            )
        target = get_definition(variant.use)
        if target is None:
            raise AgentDefinitionValidationError(
                f"Agent {definition.name!r} variant points to unknown agent "
                f"{variant.use!r}."
            )
        if target.variants:
            raise AgentDefinitionValidationError(
                f"Agent {definition.name!r} variant target {target.name!r} "
                "declares its own variants — chaining is forbidden."
            )
        if target.context_recipe and not RecipeRegistry.has(target.context_recipe):
            raise AgentDefinitionValidationError(
                f"Variant target {target.name!r} declares context_recipe="
                f"{target.context_recipe!r}, which is not registered."
            )

    # AC9 — variant-list total-coverage tail rules. The FINAL element matters
    # because the resolver is first-match-wins; an ``always``-predicate
    # anywhere but the tail position would shadow subsequent entries instead
    # of closing the partition.
    if definition.variants:
        final_when = definition.variants[-1].when
        if len(definition.variants) > 1 and final_when != _TAIL_PREDICATE:
            raise AgentDefinitionValidationError(
                f"Agent {definition.name!r} declares "
                f"{len(definition.variants)} variants but the final entry's "
                f"predicate is {final_when!r}; multi-variant lists must end "
                f"with ``when: {_TAIL_PREDICATE}`` to close the partition."
            )
        if not definition.terminals and final_when != _TAIL_PREDICATE:
            raise AgentDefinitionValidationError(
                f"Agent {definition.name!r} has no terminals of its own and "
                f"the final variant's predicate is {final_when!r}; a thin "
                f"variants-only profile must end with ``when: "
                f"{_TAIL_PREDICATE}`` so every depth resolves to a target."
            )

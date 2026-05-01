"""US-007: agents.registry.validate_agent_definitions_resolved."""

from __future__ import annotations

import pytest

from agents import registry as agents_registry
from agents.types import (
    AgentDefinition,
    AgentVariant,
)
from task_center.context_engine.errors import (
    AgentDefinitionValidationError,
)
from task_center.context_engine.predicates import PredicateRegistry
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)


@pytest.fixture(autouse=True)
def _isolate_state():
    saved_predicates = dict(PredicateRegistry._registry)
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = dict(agents_registry._DEFINITIONS)
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    yield
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    PredicateRegistry._registry.update(saved_predicates)
    RecipeRegistry._registry.update(saved_recipes)
    agents_registry._DEFINITIONS.update(saved_definitions)


def _stub_recipe(recipe_id: str) -> None:
    RecipeRegistry.register(
        ContextRecipe(
            id=recipe_id,
            required_scope_fields=frozenset({"request_id"}),
            build=lambda s, d: None,  # type: ignore[arg-type, return-value]
        )
    )


def test_unknown_predicate_id_rejected():
    _stub_recipe("planner_v1")
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        variants=[AgentVariant(when="missing_predicate", use="planner_full_only")],
    )
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner_v1",
    )
    agents_registry.register_definition(base)
    agents_registry.register_definition(full_only)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        agents_registry.validate_agent_definitions_resolved()
    assert "missing_predicate" in str(exc.value)


def test_dangling_variant_target_rejected():
    _stub_recipe("planner_v1")
    PredicateRegistry.register("p", lambda ctx: False)
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        variants=[AgentVariant(when="p", use="missing_target")],
    )
    agents_registry.register_definition(base)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        agents_registry.validate_agent_definitions_resolved()
    assert "missing_target" in str(exc.value)


def test_nested_variant_target_rejected():
    _stub_recipe("planner_v1")
    PredicateRegistry.register("p", lambda ctx: False)
    base = AgentDefinition(
        name="base",
        description="base",
        context_recipe="planner_v1",
        variants=[AgentVariant(when="p", use="middle")],
    )
    middle = AgentDefinition(
        name="middle",
        description="middle",
        context_recipe="planner_v1",
        variants=[AgentVariant(when="p", use="leaf")],
    )
    leaf = AgentDefinition(
        name="leaf", description="leaf", context_recipe="planner_v1"
    )
    for d in (base, middle, leaf):
        agents_registry.register_definition(d)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        agents_registry.validate_agent_definitions_resolved()
    assert "chaining" in str(exc.value).lower()


def test_unknown_context_recipe_rejected():
    PredicateRegistry.register("p", lambda ctx: False)
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="not_registered_recipe",
    )
    agents_registry.register_definition(base)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        agents_registry.validate_agent_definitions_resolved()
    assert "not_registered_recipe" in str(exc.value)


def test_clean_setup_passes_validation():
    _stub_recipe("planner_v1")
    _stub_recipe("generator_v1")
    PredicateRegistry.register("partial_plan_caller_ancestor", lambda ctx: False)
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        variants=[
            AgentVariant(
                when="partial_plan_caller_ancestor", use="planner_full_only"
            )
        ],
    )
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner_v1",
    )
    generator = AgentDefinition(
        name="generator",
        description="generator",
        context_recipe="generator_v1",
    )
    for d in (base, full_only, generator):
        agents_registry.register_definition(d)
    # No exception.
    agents_registry.validate_agent_definitions_resolved()


def test_definitions_with_no_recipe_pass_validation():
    """Helper / subagent definitions without context_recipe must not break
    startup — only context-engine-launched agents need a recipe."""
    legacy = AgentDefinition(name="legacy", description="legacy", context_recipe=None)
    agents_registry.register_definition(legacy)
    agents_registry.validate_agent_definitions_resolved()

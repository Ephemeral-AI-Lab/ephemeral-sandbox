"""US-007: agent definition reference validation."""

from __future__ import annotations

import pytest

from agents import (
    AgentDefinition,
    AgentVariant,
    list_definitions,
    register_definition,
    unregister_definition,
    validate_agent_definitions_resolved,
)
from agents.skills import SkillLintError
from task_center.context_engine.core import (
    AgentDefinitionValidationError,
)
from task_center._core.agent_routing import PredicateRegistry
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)


@pytest.fixture(autouse=True)
def _isolate_state():
    saved_predicates = dict(PredicateRegistry._registry)
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = list_definitions()
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    _clear_definitions()
    yield
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    _clear_definitions()
    PredicateRegistry._registry.update(saved_predicates)
    RecipeRegistry._registry.update(saved_recipes)
    for definition in saved_definitions:
        register_definition(definition)


def _clear_definitions() -> None:
    for definition in list_definitions():
        unregister_definition(definition.name)


def _stub_recipe(recipe_id: str) -> None:
    RecipeRegistry.register(
        ContextRecipe(
            id=recipe_id,
            required_scope_fields=frozenset({"request_id"}),
            build=lambda s, d: None,  # type: ignore[arg-type, return-value]
        )
    )


def test_unknown_predicate_id_rejected():
    _stub_recipe("planner")
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner",
        variants=[AgentVariant(when="missing_predicate", use="planner_full_only")],
    )
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner",
    )
    register_definition(base)
    register_definition(full_only)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        validate_agent_definitions_resolved()
    assert "missing_predicate" in str(exc.value)


def test_dangling_variant_target_rejected():
    _stub_recipe("planner")
    PredicateRegistry.register("p", lambda ctx: False)
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner",
        variants=[AgentVariant(when="p", use="missing_target")],
    )
    register_definition(base)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        validate_agent_definitions_resolved()
    assert "missing_target" in str(exc.value)


def test_nested_variant_target_rejected():
    _stub_recipe("planner")
    PredicateRegistry.register("p", lambda ctx: False)
    base = AgentDefinition(
        name="base",
        description="base",
        context_recipe="planner",
        variants=[AgentVariant(when="p", use="middle")],
    )
    middle = AgentDefinition(
        name="middle",
        description="middle",
        context_recipe="planner",
        variants=[AgentVariant(when="p", use="leaf")],
    )
    leaf = AgentDefinition(
        name="leaf", description="leaf", context_recipe="planner"
    )
    for d in (base, middle, leaf):
        register_definition(d)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        validate_agent_definitions_resolved()
    assert "chaining" in str(exc.value).lower()


def test_unknown_context_recipe_rejected():
    PredicateRegistry.register("p", lambda ctx: False)
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="not_registered_recipe",
    )
    register_definition(base)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        validate_agent_definitions_resolved()
    assert "not_registered_recipe" in str(exc.value)


def test_clean_setup_passes_validation():
    _stub_recipe("planner")
    _stub_recipe("generator")
    PredicateRegistry.register("nested_goal_depth_gt_1", lambda ctx: False)
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner",
        terminals=["submit_plan_closes_goal", "submit_plan_continues_goal"],
        variants=[
            AgentVariant(
                when="nested_goal_depth_gt_1", use="planner_full_only"
            )
        ],
    )
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner",
        terminals=["submit_plan_closes_goal"],
    )
    generator = AgentDefinition(
        name="generator",
        description="generator",
        context_recipe="generator",
        terminals=["submit_execution_success", "submit_execution_failure"],
    )
    for d in (base, full_only, generator):
        register_definition(d)
    # No exception.
    validate_agent_definitions_resolved()


def test_definitions_with_no_recipe_pass_validation():
    """Helper / subagent definitions without context_recipe must not break
    startup — only context-engine-launched agents need a recipe."""
    no_recipe = AgentDefinition(name="no_recipe", description="no recipe", context_recipe=None)
    register_definition(no_recipe)
    validate_agent_definitions_resolved()


def test_skill_lint_runs_during_resolved_validation(tmp_path):
    _stub_recipe("planner")
    skill_file = tmp_path / "planner" / "SKILL.md"
    skill_file.parent.mkdir()
    skill_file.write_text(
        "---\nname: planner\n---\n\nUse submit_plan_closes_goal here.",
        encoding="utf-8",
    )
    planner = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner",
        skill=skill_file,
    )
    register_definition(planner)

    with pytest.raises(SkillLintError) as exc:
        validate_agent_definitions_resolved()
    assert "submit_plan_closes_goal" in str(exc.value)
    assert "planner" in str(exc.value)

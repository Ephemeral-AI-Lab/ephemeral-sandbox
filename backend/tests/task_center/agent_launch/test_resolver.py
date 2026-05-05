"""US-006: PredicateRegistry + RuleBasedAgentResolver behavior."""

from __future__ import annotations

import pytest

from agents import registry as agents_registry
from agents.types import (
    AgentDefinition,
    AgentSelectionBlock,
    AgentVariant,
)
from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import (
    AgentDefinitionValidationError,
    MissingContextRecipeError,
)
from task_center.agent_launch.predicates import (
    PredicateRegistry,
    ResolverContext,
)
from task_center.agent_launch.resolver import (
    AgentSelection,
    RuleBasedAgentResolver,
)
from task_center.context_engine.scope import ContextScope


@pytest.fixture(autouse=True)
def _isolate_registries():
    saved_predicates = dict(PredicateRegistry._registry)
    saved_definitions = dict(agents_registry._DEFINITIONS)
    PredicateRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    yield
    PredicateRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    PredicateRegistry._registry.update(saved_predicates)
    agents_registry._DEFINITIONS.update(saved_definitions)


@pytest.fixture
def deps() -> ContextEngineDeps:
    class _S:
        def get(self, *a, **k):
            return None

    return ContextEngineDeps(
        request_store=_S(), segment_store=_S(),  # type: ignore[arg-type]
        graph_store=_S(), task_store=_S(),  # type: ignore[arg-type]
    )


@pytest.fixture
def planner_with_variant():
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        terminals=["submit_full_plan", "submit_partial_plan"],
        variants=[
            AgentVariant(
                when="needs_full_only",
                use="planner_full_only",
                note="ancestry has partial-plan caller",
                required_context_blocks=[
                    AgentSelectionBlock(
                        kind="launch_notice",
                        priority="required",
                        text="variant selected.",
                    )
                ],
            )
        ],
    )
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner_v1",
        terminals=["submit_full_plan"],
    )
    agents_registry.register_definition(base)
    agents_registry.register_definition(full_only)
    return base, full_only


def test_empty_variants_returns_base_fast_path(deps):
    base = AgentDefinition(
        name="generator",
        description="g",
        context_recipe="generator_v1",
    )
    agents_registry.register_definition(base)
    sel = RuleBasedAgentResolver().resolve(
        base_agent_name="generator",
        scope=ContextScope(request_id="r"),
        deps=deps,
    )
    assert isinstance(sel, AgentSelection)
    assert sel.agent_def.name == "generator"
    assert sel.context_recipe == "generator_v1"
    assert sel.required_context_blocks == ()


def test_variant_predicate_match_picks_target(deps, planner_with_variant):
    PredicateRegistry.register("needs_full_only", lambda ctx: True)
    sel = RuleBasedAgentResolver().resolve(
        base_agent_name="planner",
        scope=ContextScope(request_id="r"),
        deps=deps,
    )
    assert sel.agent_def.name == "planner_full_only"
    assert "submit_partial_plan" not in sel.agent_def.terminals
    assert len(sel.required_context_blocks) == 1
    assert sel.required_context_blocks[0].kind == "launch_notice"
    assert sel.reason == "ancestry has partial-plan caller"


def test_predicate_no_match_falls_back_to_base(deps, planner_with_variant):
    PredicateRegistry.register("needs_full_only", lambda ctx: False)
    sel = RuleBasedAgentResolver().resolve(
        base_agent_name="planner",
        scope=ContextScope(request_id="r"),
        deps=deps,
    )
    assert sel.agent_def.name == "planner"


def test_declared_order_priority(deps):
    PredicateRegistry.register("first", lambda ctx: False)
    PredicateRegistry.register("second", lambda ctx: True)
    PredicateRegistry.register("third", lambda ctx: True)
    base = AgentDefinition(
        name="x",
        description="x",
        context_recipe="x_v1",
        variants=[
            AgentVariant(when="first", use="alt_a"),
            AgentVariant(when="second", use="alt_b"),
            AgentVariant(when="third", use="alt_c"),
        ],
    )
    alt_b = AgentDefinition(name="alt_b", description="b", context_recipe="x_v1")
    alt_c = AgentDefinition(name="alt_c", description="c", context_recipe="x_v1")
    alt_a = AgentDefinition(name="alt_a", description="a", context_recipe="x_v1")
    for d in (base, alt_a, alt_b, alt_c):
        agents_registry.register_definition(d)
    sel = RuleBasedAgentResolver().resolve(
        base_agent_name="x", scope=ContextScope(request_id="r"), deps=deps
    )
    assert sel.agent_def.name == "alt_b", "first matching variant wins"


def test_nested_variant_target_rejected(deps):
    PredicateRegistry.register("always", lambda ctx: True)
    base = AgentDefinition(
        name="base",
        description="base",
        context_recipe="x_v1",
        variants=[AgentVariant(when="always", use="middle")],
    )
    middle = AgentDefinition(
        name="middle",
        description="middle",
        context_recipe="x_v1",
        variants=[AgentVariant(when="always", use="leaf")],
    )
    leaf = AgentDefinition(name="leaf", description="leaf", context_recipe="x_v1")
    for d in (base, middle, leaf):
        agents_registry.register_definition(d)
    with pytest.raises(AgentDefinitionValidationError):
        RuleBasedAgentResolver().resolve(
            base_agent_name="base",
            scope=ContextScope(request_id="r"),
            deps=deps,
        )


def test_predicate_exception_propagates_no_fail_open(deps, planner_with_variant):
    def _boom(ctx: ResolverContext) -> bool:
        raise RuntimeError("predicate exploded")

    PredicateRegistry.register("needs_full_only", _boom)
    with pytest.raises(RuntimeError):
        RuleBasedAgentResolver().resolve(
            base_agent_name="planner",
            scope=ContextScope(request_id="r"),
            deps=deps,
        )


def test_missing_context_recipe_raises(deps):
    base = AgentDefinition(name="bare", description="bare")
    agents_registry.register_definition(base)
    with pytest.raises(MissingContextRecipeError):
        RuleBasedAgentResolver().resolve(
            base_agent_name="bare",
            scope=ContextScope(request_id="r"),
            deps=deps,
        )

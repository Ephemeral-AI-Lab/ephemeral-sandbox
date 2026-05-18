"""Round 3 Phase 5: AgentSelection.skill_path propagation through the resolver."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from agents import (
    AgentDefinition,
    AgentKind,
    AgentVariant,
    register_definition,
    unregister_definition,
)
from task_center._core.agent_routing import (
    PredicateRegistry,
    RuleBasedAgentResolver,
    _always,
)
from task_center.context_engine.scope import ContextScope


@dataclass(frozen=True, slots=True)
class _Deps:
    goal_store: object = None
    iteration_store: object = None
    attempt_store: object = None
    task_store: object = None
    context_packet_store: object = None


@pytest.fixture(autouse=True)
def _isolate_predicate_registry():
    saved = dict(PredicateRegistry._registry)
    PredicateRegistry.clear()
    PredicateRegistry.register("always", _always)
    yield
    PredicateRegistry.clear()
    for name, fn in saved.items():
        PredicateRegistry.register(name, fn)


def _make_definition(
    *,
    name: str,
    skill: Path | None = None,
    variants: list[AgentVariant] | None = None,
    recipe: str = "planner",
) -> AgentDefinition:
    return AgentDefinition(
        name=name,
        description=name,
        agent_kind=AgentKind.PLANNER,
        context_recipe=recipe,
        skill=skill,
        variants=variants or [],
    )


def test_resolve_returns_skill_path_from_base_when_no_variants(
    tmp_path: Path, monkeypatch
):
    skill_file = tmp_path / "SKILL.md"
    skill_file.write_text("# planner skill")
    base = _make_definition(name="planner_test_base", skill=skill_file)
    register_definition(base)

    monkeypatch.setattr(
        "task_center.context_engine.recipes_registry.RecipeRegistry.get",
        lambda key: MagicMock(),
    )

    selection = RuleBasedAgentResolver().resolve(
        base_agent_name="planner_test_base",
        scope=ContextScope.for_planner(
            goal_id="g", iteration_id="i", attempt_id="a"
        ),
        deps=_Deps(),  # type: ignore[arg-type]
    )

    assert selection.skill_path == skill_file
    assert selection.agent_def.name == "planner_test_base"
    unregister_definition("planner_test_base")


def test_resolve_returns_target_skill_when_variant_fires(
    tmp_path: Path, monkeypatch
):
    base_skill = tmp_path / "base.md"
    base_skill.write_text("# base skill")
    target_skill = tmp_path / "target.md"
    target_skill.write_text("# target skill")

    target = _make_definition(name="planner_test_target", skill=target_skill)
    base = _make_definition(
        name="planner_test_router",
        skill=base_skill,
        variants=[AgentVariant(when="always", use="planner_test_target")],
    )
    register_definition(target)
    register_definition(base)

    monkeypatch.setattr(
        "task_center.context_engine.recipes_registry.RecipeRegistry.get",
        lambda key: MagicMock(),
    )

    selection = RuleBasedAgentResolver().resolve(
        base_agent_name="planner_test_router",
        scope=ContextScope.for_planner(
            goal_id="g", iteration_id="i", attempt_id="a"
        ),
        deps=_Deps(),  # type: ignore[arg-type]
    )

    assert selection.skill_path == target_skill
    assert selection.agent_def.name == "planner_test_target"

    unregister_definition("planner_test_target")
    unregister_definition("planner_test_router")


def test_resolve_returns_none_when_no_skill_declared(monkeypatch):
    plain = _make_definition(name="planner_test_plain", skill=None)
    register_definition(plain)
    monkeypatch.setattr(
        "task_center.context_engine.recipes_registry.RecipeRegistry.get",
        lambda key: MagicMock(),
    )

    selection = RuleBasedAgentResolver().resolve(
        base_agent_name="planner_test_plain",
        scope=ContextScope.for_planner(
            goal_id="g", iteration_id="i", attempt_id="a"
        ),
        deps=_Deps(),  # type: ignore[arg-type]
    )

    assert selection.skill_path is None
    unregister_definition("planner_test_plain")

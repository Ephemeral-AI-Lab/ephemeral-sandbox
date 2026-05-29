"""TerminalToolSelection skill path propagation through terminal routing."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from agents import (
    AgentDefinition,
    AgentKind,
    list_definitions,
    register_definition,
    unregister_definition,
)
from task_center._core.terminal_tool_routing import TerminalToolRouter
from task_center.context_engine.scope import ContextScope


@dataclass(frozen=True, slots=True)
class _Deps:
    workflow_store: object = None
    iteration_store: object = None
    attempt_store: object = None
    task_store: object = None
    context_packet_store: object = None


@pytest.fixture(autouse=True)
def _isolate_agent_definitions():
    saved = list_definitions()
    for definition in saved:
        unregister_definition(definition.name)
    yield
    for definition in list_definitions():
        unregister_definition(definition.name)
    for definition in saved:
        register_definition(definition)


def _make_definition(
    *,
    name: str,
    skill: Path | None = None,
    terminals: list[str] | None = None,
    recipe: str = "planner",
) -> AgentDefinition:
    return AgentDefinition(
        name=name,
        description=name,
        agent_kind=AgentKind.PLANNER,
        context_recipe=recipe,
        skill=skill,
        terminals=terminals or ["submit_x"],
        tool_call_limit=10,
    )


def test_resolve_returns_skill_path_from_registered_definition(
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

    selection = TerminalToolRouter().resolve(
        base_agent_name="planner_test_base",
        scope=ContextScope(),
        deps=_Deps(),  # type: ignore[arg-type]
    )

    assert selection.skill_path == skill_file
    assert selection.agent_def.name == "planner_test_base"
    unregister_definition("planner_test_base")


def test_resolve_keeps_base_skill_when_terminals_are_filtered(
    tmp_path: Path, monkeypatch
):
    base_skill = tmp_path / "SKILL.md"
    base_skill.write_text("# planner skill")
    base = _make_definition(
        name="planner_test_router",
        skill=base_skill,
        terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
    )
    register_definition(base)

    monkeypatch.setattr(
        "task_center.context_engine.recipes_registry.RecipeRegistry.get",
        lambda key: MagicMock(),
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="planner_test_router",
        scope=ContextScope.for_planner(
            workflow_id=None, iteration_id="i", attempt_id="a"
        ),
        deps=_Deps(),  # type: ignore[arg-type]
    )

    assert selection.skill_path == base_skill
    assert selection.agent_def.name == "planner_test_router"

    unregister_definition("planner_test_router")


def test_resolve_returns_none_when_no_skill_declared(monkeypatch):
    plain = _make_definition(name="planner_test_plain", skill=None)
    register_definition(plain)
    monkeypatch.setattr(
        "task_center.context_engine.recipes_registry.RecipeRegistry.get",
        lambda key: MagicMock(),
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="planner_test_plain",
        scope=ContextScope(),
        deps=_Deps(),  # type: ignore[arg-type]
    )

    assert selection.skill_path is None
    unregister_definition("planner_test_plain")

"""TerminalToolRouter behavior."""

from __future__ import annotations

import pytest

from agents import (
    AgentDefinition,
    AgentKind,
    list_definitions,
    register_definition,
    unregister_definition,
)
from task_center._core.terminal_tool_routing import (
    TerminalToolRouter,
    TerminalToolSelection,
)
from task_center.context_engine.core import ContextEngineDeps, MissingContextRecipeError
from task_center.context_engine.scope import ContextScope


@pytest.fixture(autouse=True)
def _isolate_registries():
    saved_definitions = list_definitions()
    _clear_definitions()
    yield
    _clear_definitions()
    for definition in saved_definitions:
        register_definition(definition)


def _clear_definitions() -> None:
    for definition in list_definitions():
        unregister_definition(definition.name)


@pytest.fixture
def deps() -> ContextEngineDeps:
    class _S:
        def get(self, *_args, **_kwargs):
            return None

    return ContextEngineDeps(
        workflow_store=_S(),  # type: ignore[arg-type]
        iteration_store=_S(),  # type: ignore[arg-type]
        attempt_store=_S(),  # type: ignore[arg-type]
        task_store=_S(),  # type: ignore[arg-type]
    )


def _register(
    *,
    name: str,
    kind: AgentKind,
    terminals: list[str],
    recipe: str = "recipe",
) -> AgentDefinition:
    definition = AgentDefinition(
        name=name,
        description=name,
        agent_kind=kind,
        context_recipe=recipe,
        terminals=terminals,
        tool_call_limit=10,
    )
    register_definition(definition)
    return definition


def test_router_returns_effective_copy_without_mutating_registered_definition(
    deps, monkeypatch
):
    original = _register(
        name="planner",
        kind=AgentKind.PLANNER,
        terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
        recipe="planner",
    )
    monkeypatch.setattr(
        "task_center._core.terminal_tool_routing._nested_workflow_depth_gt_1",
        lambda ctx: True,
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="planner",
        scope=ContextScope(workflow_id="g"),
        deps=deps,
    )

    assert isinstance(selection, TerminalToolSelection)
    assert selection.agent_def.name == "planner"
    assert selection.agent_def.terminals == ["submit_plan_closes_goal"]
    assert original.terminals == ["submit_plan_closes_goal", "submit_plan_defers_goal"]


def test_planner_depth_zero_or_one_keeps_close_and_defer(deps, monkeypatch):
    _register(
        name="planner",
        kind=AgentKind.PLANNER,
        terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
        recipe="planner",
    )
    monkeypatch.setattr(
        "task_center._core.terminal_tool_routing._nested_workflow_depth_gt_1",
        lambda ctx: False,
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="planner",
        scope=ContextScope(workflow_id="g"),
        deps=deps,
    )

    assert selection.agent_def.terminals == [
        "submit_plan_closes_goal",
        "submit_plan_defers_goal",
    ]


def test_executor_depth_gt_one_filters_handoff(deps, monkeypatch):
    _register(
        name="executor",
        kind=AgentKind.EXECUTOR,
        terminals=[
            "submit_execution_handoff",
            "submit_execution_success",
            "submit_execution_blocker",
        ],
        recipe="generator",
    )
    monkeypatch.setattr(
        "task_center._core.terminal_tool_routing._nested_workflow_depth_gt_1",
        lambda ctx: True,
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="executor",
        scope=ContextScope(workflow_id="g"),
        deps=deps,
    )

    assert selection.agent_def.terminals == [
        "submit_execution_success",
        "submit_execution_blocker",
    ]


def test_executor_depth_zero_or_one_keeps_handoff(deps, monkeypatch):
    _register(
        name="executor",
        kind=AgentKind.EXECUTOR,
        terminals=[
            "submit_execution_handoff",
            "submit_execution_success",
            "submit_execution_blocker",
        ],
        recipe="generator",
    )
    monkeypatch.setattr(
        "task_center._core.terminal_tool_routing._nested_workflow_depth_gt_1",
        lambda ctx: False,
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="executor",
        scope=ContextScope(workflow_id="g"),
        deps=deps,
    )

    assert selection.agent_def.terminals == [
        "submit_execution_handoff",
        "submit_execution_success",
        "submit_execution_blocker",
    ]


def test_executor_without_goal_keeps_registered_terminals(deps, monkeypatch):
    _register(
        name="standalone_executor",
        kind=AgentKind.EXECUTOR,
        terminals=["submit_execution_success"],
        recipe="generator",
    )
    monkeypatch.setattr(
        "task_center._core.terminal_tool_routing._nested_workflow_depth_gt_1",
        lambda ctx: True,
    )

    selection = TerminalToolRouter().resolve(
        base_agent_name="standalone_executor",
        scope=ContextScope(workflow_id=None),
        deps=deps,
    )

    assert selection.agent_def.terminals == ["submit_execution_success"]


def test_missing_context_recipe_raises(deps):
    register_definition(
        AgentDefinition(
            name="bare",
            description="bare",
            terminals=["submit_x"],
            tool_call_limit=10,
        )
    )
    with pytest.raises(MissingContextRecipeError):
        TerminalToolRouter().resolve(
            base_agent_name="bare",
            scope=ContextScope(workflow_id="g"),
            deps=deps,
        )

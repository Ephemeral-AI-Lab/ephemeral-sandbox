"""Acceptance tests for agent dispatch and terminal-routing cleanup.

These pin the live invariants after profile variants were removed: planner
dispatchability is explicit, validation accepts the single planner shape, and
runtime metadata still exposes exact agent kinds.
"""

from __future__ import annotations

import pytest

from agents import (
    AgentDefinition,
    AgentKind,
    list_definitions,
    register_definition,
    unregister_definition,
    validate_agent_definitions_resolved,
)
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)
from tools.submission.planner._schemas import _is_generator_capable_agent


@pytest.fixture(autouse=True)
def _isolate():
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = list_definitions()
    RecipeRegistry.clear()
    for d in list_definitions():
        unregister_definition(d.name)
    yield
    RecipeRegistry.clear()
    for d in list_definitions():
        unregister_definition(d.name)
    RecipeRegistry._registry.update(saved_recipes)
    for d in saved_definitions:
        register_definition(d)


def _stub_recipe(recipe_id: str) -> None:
    RecipeRegistry.register(
        ContextRecipe(
            id=recipe_id,
            required_scope_fields=frozenset({"request_id"}),
            build=lambda s, d: None,  # type: ignore[arg-type, return-value]
        )
    )


# ---------------------------------------------------------------------------
# AC2 — planner submission gate: only executor / verifier kinds are
# generator-capable.
# ---------------------------------------------------------------------------


def test_ac2_executor_is_accepted() -> None:
    register_definition(
        AgentDefinition(
            name="executor_alt",
            description="alt executor",
            terminals=["submit_x"],
            tool_call_limit=10,
            agent_kind=AgentKind.EXECUTOR,
        )
    )
    assert _is_generator_capable_agent("executor_alt") is True


def test_ac2_verifier_is_accepted() -> None:
    register_definition(
        AgentDefinition(
            name="verifier_alt",
            description="alt verifier",
            terminals=["submit_x"],
            tool_call_limit=10,
            agent_kind=AgentKind.VERIFIER,
        )
    )
    assert _is_generator_capable_agent("verifier_alt") is True


def test_ac2_non_generator_kind_is_rejected() -> None:
    register_definition(
        AgentDefinition(
            name="advisor_alt",
            description="alt advisor",
            terminals=["submit_x"],
            tool_call_limit=10,
            agent_kind=AgentKind.ADVISOR,
        )
    )
    assert _is_generator_capable_agent("advisor_alt") is False


def test_ac2_unknown_agent_name_is_rejected() -> None:
    assert _is_generator_capable_agent("never_registered") is False


def test_ac9_planner_md_shape_passes_validation() -> None:
    """The planner.md shape validates without legacy profile variants."""
    _stub_recipe("planner")
    planner = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner",
        terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
        tool_call_limit=10,
    )
    register_definition(planner)
    # Must not raise.
    validate_agent_definitions_resolved()


# ---------------------------------------------------------------------------
# AC10 — audit-shape regression. The metadata["role"] value emitted by the
# factory MUST equal agent_def.agent_kind.value exactly (set membership is
# not enough; downstream audit consumers index by the exact string).
# ---------------------------------------------------------------------------


def test_ac10_factory_metadata_role_matches_agent_kind_value_exactly() -> None:
    from types import SimpleNamespace

    from engine.agent.factory import _build_agent_tool_registry

    for kind in AgentKind:
        agent_def = AgentDefinition(
            name=f"ac10_{kind.value}",
            description="ac10 fixture",
            agent_kind=kind,
            allowed_tools=[],
            terminals=["submit_execution_success"],
            tool_call_limit=10,
        )
        registry_metadata: list[dict] = []

        from tools._framework.factory import (
            _factories,
            register_tool_factory,
        )

        _factories.pop("ac10_probe", None)

        def _factory(ctx, _metadata=registry_metadata):
            _metadata.append(dict(ctx.metadata))
            from tools._framework.core.base import (
                BaseTool,
                ToolExecutionContextService,
                ToolResult,
            )
            from pydantic import BaseModel

            class _Empty(BaseModel):
                pass

            class _Probe(BaseTool):
                name = "ac10_probe"
                description = "probe"
                input_model = _Empty

                async def execute(
                    self,
                    arguments: BaseModel,
                    context: ToolExecutionContextService,
                ) -> ToolResult:
                    del arguments, context
                    return ToolResult(output="ok")

            return _Probe()

        register_tool_factory("ac10_probe", _factory)
        try:
            agent_def_with_probe = AgentDefinition(
                name=agent_def.name,
                description=agent_def.description,
                agent_kind=agent_def.agent_kind,
                allowed_tools=["ac10_probe"],
                terminals=agent_def.terminals,
                tool_call_limit=10,
            )
            _build_agent_tool_registry(
                SimpleNamespace(cwd="/tmp"),
                agent_def_with_probe,
                None,
                agent_def_with_probe.name,
            )
        finally:
            _factories.pop("ac10_probe", None)

        assert registry_metadata, "tool factory should have been invoked"
        captured_role = registry_metadata[-1]["role"]
        assert captured_role == kind.value, (
            f"metadata['role']={captured_role!r} must equal "
            f"agent_kind.value={kind.value!r} exactly"
        )

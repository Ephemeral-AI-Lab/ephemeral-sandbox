"""Acceptance tests for the agent-router replan (AC2 / AC7 / AC8 / AC9 / AC10).

These pin the cross-cutting invariants the depth-based variant routing relies
on. Each test is keyed to an acceptance criterion in
``.planning/agent-router-replan.md``.
"""

from __future__ import annotations

import pytest

from agents import (
    AgentDefinition,
    AgentKind,
    AgentVariant,
    list_definitions,
    register_definition,
    unregister_definition,
    validate_agent_definitions_resolved,
)
from task_center._core.agent_routing import (
    PredicateRegistry,
    ResolverContext,
    register_builtin_predicates,
)
from task_center.context_engine.core import ContextEngineDeps, AgentDefinitionValidationError
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)
from task_center.context_engine.scope import ContextScope
from tools.submission.planner._schemas import _is_generator_capable_agent


@pytest.fixture(autouse=True)
def _isolate():
    saved_predicates = dict(PredicateRegistry._registry)
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = list_definitions()
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    for d in list_definitions():
        unregister_definition(d.name)
    register_builtin_predicates()
    yield
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    for d in list_definitions():
        unregister_definition(d.name)
    PredicateRegistry._registry.update(saved_predicates)
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
# AC2 — planner submission gate: dispatchable_by_planner is required and
# applies even to agents whose agent_kind is executor (closes the entry_
# executor hole the literal-name fast-path used to mask).
# ---------------------------------------------------------------------------


def test_ac2_dispatchable_executor_is_accepted() -> None:
    register_definition(
        AgentDefinition(
            name="executor_alt",
            description="alt executor",
            agent_kind=AgentKind.EXECUTOR,
            dispatchable_by_planner=True,
        )
    )
    assert _is_generator_capable_agent("executor_alt") is True


def test_ac2_non_dispatchable_executor_is_rejected() -> None:
    register_definition(
        AgentDefinition(
            name="executor_alt",
            description="alt executor",
            agent_kind=AgentKind.EXECUTOR,
            dispatchable_by_planner=False,
        )
    )
    assert _is_generator_capable_agent("executor_alt") is False


def test_ac2_entry_executor_is_rejected() -> None:
    """Regression for the advisor-caught bug.

    Before Stage 6 the literal-name fast-path (``agent_name in {"executor",
    "verifier"}``) plus entry_executor's ``agent_kind: executor`` would have
    let a planner submit ``agent_name="entry_executor"``. After Stage 6 the
    gate requires explicit ``dispatchable_by_planner=True``, which
    entry_executor.md does not set.
    """
    register_definition(
        AgentDefinition(
            name="entry_executor",
            description="entry executor",
            agent_kind=AgentKind.EXECUTOR,
            # dispatchable_by_planner defaults to False — that is the contract.
        )
    )
    assert _is_generator_capable_agent("entry_executor") is False


def test_ac2_unknown_agent_name_is_rejected() -> None:
    assert _is_generator_capable_agent("never_registered") is False


# ---------------------------------------------------------------------------
# AC7 — variant partition is mechanically total for the executor profile.
# Every depth in {0,1,2,3,4} resolves to exactly one variant target.
# ---------------------------------------------------------------------------


def test_ac7_executor_variant_disjunction_total_across_depths(monkeypatch) -> None:
    """Every depth d in {0..4} satisfies EXACTLY ONE of the executor's two
    variant predicates — within / above partition the depth axis."""
    fake_depth = {"value": 0}

    def _fake_nested_goal_depth(**_kwargs) -> int:
        return fake_depth["value"]

    monkeypatch.setattr(
        "task_center._core.agent_routing.nested_goal_depth",
        _fake_nested_goal_depth,
    )

    class _S:
        def get(self, *_args, **_kwargs):
            return None

    deps = ContextEngineDeps(
        goal_store=_S(),  # type: ignore[arg-type]
        iteration_store=_S(),  # type: ignore[arg-type]
        attempt_store=_S(),  # type: ignore[arg-type]
        task_store=_S(),  # type: ignore[arg-type]
    )
    ctx = ResolverContext(scope=ContextScope(goal_id="m"), deps=deps)

    within = PredicateRegistry.get(
        "nested_goal_depth_within_handoff_range"
    )
    above = PredicateRegistry.get(
        "nested_goal_depth_above_handoff_range"
    )

    for depth in range(5):
        fake_depth["value"] = depth
        assert within(ctx) ^ above(ctx), (
            f"depth {depth} must satisfy exactly one of within/above"
        )


# ---------------------------------------------------------------------------
# AC8 — the ``always`` predicate is registered and unconditionally True.
# ---------------------------------------------------------------------------


def test_ac8_always_predicate_is_registered_and_unconditional() -> None:
    pred = PredicateRegistry.get("always")

    class _S:
        def get(self, *_args, **_kwargs):
            return None

    deps = ContextEngineDeps(
        goal_store=_S(),  # type: ignore[arg-type]
        iteration_store=_S(),  # type: ignore[arg-type]
        attempt_store=_S(),  # type: ignore[arg-type]
        task_store=_S(),  # type: ignore[arg-type]
    )
    assert pred(ResolverContext(scope=ContextScope(), deps=deps)) is True
    assert (
        pred(ResolverContext(scope=ContextScope(goal_id="x"), deps=deps))
        is True
    )


# ---------------------------------------------------------------------------
# AC9 — variant-list total-coverage tail rules raise AgentDefinitionValidation
# Error on malformed profiles. The "final element" wording matters because
# resolver is first-match-wins.
# ---------------------------------------------------------------------------


def test_ac9_multi_variant_without_always_tail_rejected() -> None:
    _stub_recipe("generator")
    PredicateRegistry.register("p_a", lambda ctx: False)
    PredicateRegistry.register("p_b", lambda ctx: False)
    leaf = AgentDefinition(
        name="leaf_a",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    leaf_b = AgentDefinition(
        name="leaf_b",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    base = AgentDefinition(
        name="thin_base",
        description="base",
        context_recipe="generator",
        terminals=["submit_execution_success"],
        variants=[
            AgentVariant(when="p_a", use="leaf_a"),
            AgentVariant(when="p_b", use="leaf_b"),
        ],
    )
    for d in (leaf, leaf_b, base):
        register_definition(d)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        validate_agent_definitions_resolved()
    assert "always" in str(exc.value)


def test_ac9_thin_variants_only_without_always_tail_rejected() -> None:
    _stub_recipe("generator")
    PredicateRegistry.register("p_a", lambda ctx: False)
    leaf = AgentDefinition(
        name="leaf_a",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    base = AgentDefinition(
        name="thin_base",
        description="base",
        context_recipe="generator",
        # No terminals — relies on variants to cover every input.
        variants=[AgentVariant(when="p_a", use="leaf_a")],
    )
    for d in (leaf, base):
        register_definition(d)
    with pytest.raises(AgentDefinitionValidationError) as exc:
        validate_agent_definitions_resolved()
    assert "always" in str(exc.value)


def test_ac9_always_predicate_must_be_in_tail_position_for_multi_variant() -> None:
    _stub_recipe("generator")
    PredicateRegistry.register("always", lambda ctx: True)
    PredicateRegistry.register("p_b", lambda ctx: False)
    leaf_a = AgentDefinition(
        name="leaf_a",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    leaf_b = AgentDefinition(
        name="leaf_b",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    base = AgentDefinition(
        name="thin_base",
        description="base",
        context_recipe="generator",
        terminals=["submit_execution_success"],
        # "always" shadows the later variant — final element matters.
        variants=[
            AgentVariant(when="always", use="leaf_a"),
            AgentVariant(when="p_b", use="leaf_b"),
        ],
    )
    for d in (leaf_a, leaf_b, base):
        register_definition(d)
    with pytest.raises(AgentDefinitionValidationError):
        validate_agent_definitions_resolved()


def test_ac9_thin_variants_only_with_non_always_final_rejected() -> None:
    _stub_recipe("generator")
    PredicateRegistry.register("always", lambda ctx: True)
    PredicateRegistry.register("p_b", lambda ctx: False)
    leaf_a = AgentDefinition(
        name="leaf_a",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    leaf_b = AgentDefinition(
        name="leaf_b",
        description="leaf",
        context_recipe="generator",
        terminals=["submit_execution_success"],
    )
    base = AgentDefinition(
        name="thin_base",
        description="base",
        context_recipe="generator",
        # Empty terminals + variants not ending with always — the
        # tail-position rule catches the silent no-match risk.
        variants=[
            AgentVariant(when="always", use="leaf_a"),
            AgentVariant(when="p_b", use="leaf_b"),
        ],
    )
    for d in (leaf_a, leaf_b, base):
        register_definition(d)
    with pytest.raises(AgentDefinitionValidationError):
        validate_agent_definitions_resolved()


def test_ac9_planner_md_shape_passes_validation() -> None:
    """The planner.md shape — single variant + non-empty terminals — is the
    paradigmatic passing case: terminals cover the no-match branch, no
    ``always`` tail required."""
    _stub_recipe("planner")
    PredicateRegistry.register("nested_goal_depth_gt_1", lambda ctx: False)
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner",
        terminals=["submit_full_plan"],
    )
    planner = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner",
        terminals=["submit_full_plan", "submit_partial_plan"],
        variants=[
            AgentVariant(
                when="nested_goal_depth_gt_1", use="planner_full_only"
            )
        ],
    )
    register_definition(planner)
    register_definition(full_only)
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

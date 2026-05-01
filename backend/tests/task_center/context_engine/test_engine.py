"""US-004: ContextEngine routing + RecipeRegistry behavior."""

from __future__ import annotations

import pytest

from task_center.context_engine.engine import ContextEngine, ContextEngineDeps
from task_center.context_engine.errors import (
    ContextEngineError,
    RecipeScopeError,
)
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)
from task_center.context_engine.scope import ContextScope


@pytest.fixture(autouse=True)
def _isolate_registry():
    """Each test starts with a fresh registry."""
    saved = dict(RecipeRegistry._registry)
    RecipeRegistry.clear()
    yield
    RecipeRegistry.clear()
    RecipeRegistry._registry.update(saved)


@pytest.fixture
def deps() -> ContextEngineDeps:
    # Recipes in these tests do not call any store, so simple stubs suffice.
    class _Stub:
        def get(self, *args, **kwargs):
            return None

    return ContextEngineDeps(
        request_store=_Stub(),  # type: ignore[arg-type]
        segment_store=_Stub(),  # type: ignore[arg-type]
        graph_store=_Stub(),  # type: ignore[arg-type]
        task_store=_Stub(),  # type: ignore[arg-type]
    )


def _ok_recipe(recipe_id: str, *, required: frozenset[str]) -> ContextRecipe:
    def _build(scope: ContextScope, deps: ContextEngineDeps) -> ContextPacket:
        return ContextPacket(
            target_role="planner",
            target_id=scope.harness_graph_id,
            canonical_refs=ContextRefs(
                request_id=scope.request_id,
                segment_id=scope.segment_id,
                harness_graph_id=scope.harness_graph_id,
            ),
            blocks=[
                ContextBlock(
                    kind="segment_goal",
                    priority=ContextPriority.REQUIRED,
                    text="ok",
                )
            ],
        )

    return ContextRecipe(id=recipe_id, required_scope_fields=required, build=_build)


def test_unknown_recipe_id_raises_at_build(deps):
    engine = ContextEngine(deps)
    with pytest.raises(ContextEngineError):
        engine.build("missing", ContextScope(request_id="r"))


def test_engine_validates_scope_before_calling_recipe(deps):
    RecipeRegistry.register(
        _ok_recipe("r1", required=frozenset({"request_id", "segment_id"}))
    )
    with pytest.raises(RecipeScopeError):
        ContextEngine(deps).build("r1", ContextScope(request_id="r"))


def test_engine_dispatches_to_registered_recipe(deps):
    RecipeRegistry.register(
        _ok_recipe(
            "r1",
            required=frozenset({"request_id", "segment_id", "harness_graph_id"}),
        )
    )
    packet = ContextEngine(deps).build(
        "r1",
        ContextScope(request_id="r", segment_id="s", harness_graph_id="g"),
    )
    assert packet.target_id == "g"
    assert packet.canonical_refs.request_id == "r"


def test_recipe_registry_list_ids_returns_sorted():
    RecipeRegistry.register(
        ContextRecipe(
            id="b", required_scope_fields=frozenset(), build=lambda s, d: None  # type: ignore[arg-type]
        )
    )
    RecipeRegistry.register(
        ContextRecipe(
            id="a", required_scope_fields=frozenset(), build=lambda s, d: None  # type: ignore[arg-type]
        )
    )
    assert RecipeRegistry.list_ids() == ["a", "b"]

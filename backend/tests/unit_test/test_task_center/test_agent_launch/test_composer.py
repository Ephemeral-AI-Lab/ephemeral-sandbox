"""US-012: ContextComposer single-method orchestration."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

import db.models  # noqa: F401
from agents import (
    AgentDefinition,
    AgentSelectionBlock,
    AgentVariant,
    list_definitions,
    register_definition,
    unregister_definition,
)
from db.base import Base
from db.stores.context_packet_store import ContextPacketStore
from task_center.agent_launch.composer import ContextComposer, LaunchBundle
from task_center.context_engine.engine import ContextEngine, ContextEngineDeps
from task_center.context_engine.errors import MissingContextRecipeError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.agent_launch.predicates import PredicateRegistry
from task_center.context_engine.recipes_registry import (
    ContextRecipe,
    RecipeRegistry,
)
from task_center.agent_launch.resolver import RuleBasedAgentResolver
from task_center.context_engine.scope import ContextScope


@pytest.fixture(autouse=True)
def _isolate():
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


@pytest.fixture
def packet_store():
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    store = ContextPacketStore()
    store.initialize(sf)
    yield store
    engine.dispose()


def _ok_recipe(recipe_id: str):
    def _build(scope: ContextScope, deps: ContextEngineDeps) -> ContextPacket:
        return ContextPacket(
            target_role="planner",
            target_id=scope.attempt_id,
            canonical_refs=ContextRefs(
                mission_id=scope.mission_id,
                episode_id=scope.episode_id,
                attempt_id=scope.attempt_id,
            ),
            blocks=[
                ContextBlock(
                    kind="episode_goal",
                    priority=ContextPriority.REQUIRED,
                    text="goal",
                )
            ],
        )

    return ContextRecipe(
        id=recipe_id,
        required_scope_fields=frozenset(
            {"mission_id", "episode_id", "attempt_id"}
        ),
        build=_build,
    )


def _stub_deps(packet_store) -> ContextEngineDeps:
    class _S:
        def get(self, *_args, **_kwargs):
            return None

    return ContextEngineDeps(
        mission_store=_S(),  # type: ignore[arg-type]
        episode_store=_S(),  # type: ignore[arg-type]
        attempt_store=_S(),  # type: ignore[arg-type]
        task_store=_S(),  # type: ignore[arg-type]
        context_packet_store=packet_store,
    )


def test_compose_threads_calls_in_order(packet_store):
    RecipeRegistry.register(_ok_recipe("planner_v1"))
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        system_prompt="SYSTEM PROMPT",
    )
    register_definition(base)
    deps = _stub_deps(packet_store)
    composer = ContextComposer.default(ContextEngine(deps))
    bundle = composer.compose(
        base_agent_name="planner",
        scope=ContextScope(
            mission_id="r", episode_id="s", attempt_id="g"
        ),
    )
    assert isinstance(bundle, LaunchBundle)
    assert bundle.agent_def.name == "planner"
    assert bundle.agent_def.system_prompt == "SYSTEM PROMPT"
    assert bundle.context_packet_id is not None
    assert "Current Episode" in bundle.rendered_prompt
    # Packet was persisted.
    assert packet_store.get(bundle.context_packet_id) is not None


def test_required_context_blocks_appended_before_render(packet_store):
    PredicateRegistry.register("always", lambda ctx: True)
    RecipeRegistry.register(_ok_recipe("planner_v1"))
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        variants=[
            AgentVariant(
                when="always",
                use="planner_full_only",
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
        system_prompt="FULL ONLY",
    )
    register_definition(base)
    register_definition(full_only)

    deps = _stub_deps(packet_store)
    composer = ContextComposer.default(ContextEngine(deps))
    bundle = composer.compose(
        base_agent_name="planner",
        scope=ContextScope(
            mission_id="r", episode_id="s", attempt_id="g"
        ),
    )
    assert bundle.agent_def.name == "planner_full_only"
    kinds = [b.kind for b in bundle.packet.blocks]
    assert "launch_notice" in kinds
    assert "variant selected." in bundle.rendered_prompt


def test_compose_persists_packet_only_with_store():
    """When deps.context_packet_store is None, composer skips persistence."""
    RecipeRegistry.register(_ok_recipe("planner_v1"))
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
    )
    register_definition(base)

    class _S:
        def get(self, *_args, **_kwargs):
            return None

    deps = ContextEngineDeps(
        mission_store=_S(),  # type: ignore[arg-type]
        episode_store=_S(),  # type: ignore[arg-type]
        attempt_store=_S(),  # type: ignore[arg-type]
        task_store=_S(),  # type: ignore[arg-type]
        context_packet_store=None,
    )
    composer = ContextComposer.default(ContextEngine(deps))
    bundle = composer.compose(
        base_agent_name="planner",
        scope=ContextScope(
            mission_id="r", episode_id="s", attempt_id="g"
        ),
    )
    assert bundle.context_packet_id is None


def test_resolver_engine_renderer_called_with_correct_args(packet_store):
    """Mock resolver/engine/renderer and assert the wiring contract."""
    RecipeRegistry.register(_ok_recipe("planner_v1"))
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        system_prompt="P",
    )
    register_definition(base)

    deps = _stub_deps(packet_store)
    engine = ContextEngine(deps)
    renderer = MagicMock()
    renderer.render.return_value = "RENDERED"
    composer = ContextComposer(
        resolver=RuleBasedAgentResolver(),
        engine=engine,
        renderer=renderer,
    )

    scope = ContextScope(
        mission_id="r", episode_id="s", attempt_id="g"
    )
    bundle = composer.compose(base_agent_name="planner", scope=scope)
    renderer.render.assert_called_once()
    rendered_packet = renderer.render.call_args[0][0]
    assert isinstance(rendered_packet, ContextPacket)
    assert bundle.rendered_prompt == "RENDERED"


def test_missing_context_recipe_raises_before_render(packet_store):
    base = AgentDefinition(name="bare", description="bare")
    register_definition(base)
    deps = _stub_deps(packet_store)
    composer = ContextComposer.default(ContextEngine(deps))
    with pytest.raises(MissingContextRecipeError):
        composer.compose(
            base_agent_name="bare",
            scope=ContextScope(mission_id="r"),
        )

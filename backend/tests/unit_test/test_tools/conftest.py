"""Fixtures for tool tests that need TaskCenter stores."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from agents import (
    AgentDefinition,
    AgentKind,
    list_definitions,
    register_definition,
    unregister_definition,
)
from db.base import Base
import db.models  # noqa: F401
from db.models.task_center import TaskCenterRequestRecord, TaskCenterRunRecord
from db.stores.workflow_store import WorkflowStore
from db.stores.context_packet_store import ContextPacketStore
from db.stores.attempt_store import AttemptStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.iteration_store import IterationStore
from task_center.agent_launch.composer import AgentEntryComposer
from task_center.context_engine.core import ContextEngine, ContextEngineDeps
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.recipes_registry import RecipeRegistry


@pytest.fixture
def session_factory():
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    with sf() as session:
        session.add(
            TaskCenterRequestRecord(
                id="req1",
                cwd="/tmp",
                sandbox_id=None,
                request_prompt="prompt",
                created_at=datetime.now(UTC),
                updated_at=datetime.now(UTC),
            )
        )
        session.add(
            TaskCenterRunRecord(
                id="run1",
                request_id="req1",
                status="running",
                started_at=datetime.now(UTC),
            )
        )
        session.commit()
    yield sf
    engine.dispose()


@pytest.fixture
def workflow_store(session_factory) -> WorkflowStore:
    store = WorkflowStore()
    store.initialize(session_factory)
    return store


@pytest.fixture
def iteration_store(session_factory) -> IterationStore:
    store = IterationStore()
    store.initialize(session_factory)
    return store


@pytest.fixture
def attempt_store(session_factory) -> AttemptStore:
    store = AttemptStore()
    store.initialize(session_factory)
    return store


@pytest.fixture
def task_store(session_factory) -> TaskCenterStore:
    store = TaskCenterStore()
    store.initialize(session_factory)
    return store


@pytest.fixture
def context_packet_store(session_factory) -> ContextPacketStore:
    store = ContextPacketStore()
    store.initialize(session_factory)
    return store


@pytest.fixture
def isolated_agent_registries():
    """Save + restore recipe / agent registries for test isolation."""
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = list_definitions()
    RecipeRegistry.clear()
    _clear_definitions()
    register_builtin_recipes()
    yield
    RecipeRegistry.clear()
    _clear_definitions()
    RecipeRegistry._registry.update(saved_recipes)
    for definition in saved_definitions:
        register_definition(definition)


def _clear_definitions() -> None:
    for definition in list_definitions():
        unregister_definition(definition.name)


@pytest.fixture
def register_test_agents(isolated_agent_registries):
    """Register the bare-minimum agents needed by tool/lifecycle tests."""
    register_definition(
        AgentDefinition(
            name="planner",
            description="test planner",
            agent_kind=AgentKind.PLANNER,
            context_recipe="planner",
            terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
            tool_call_limit=10,
        )
    )
    register_definition(
        AgentDefinition(
            name="executor",
            description="test executor",
            tool_call_limit=10,
            agent_kind=AgentKind.EXECUTOR,
            dispatchable_by_planner=True,
            context_recipe="generator",
            terminals=[
                "submit_execution_handoff",
                "submit_execution_success",
                "submit_execution_blocker",
            ],
        )
    )
    register_definition(
        AgentDefinition(
            name="generator",
            description="test generator",
            agent_kind=AgentKind.EXECUTOR,
            context_recipe="generator",
            terminals=["submit_execution_success", "submit_execution_blocker"],
            tool_call_limit=10,
        )
    )
    register_definition(
        AgentDefinition(
            name="verifier",
            description="test verifier",
            agent_kind=AgentKind.EXECUTOR,
            dispatchable_by_planner=True,
            context_recipe="generator",
            terminals=["submit_verification_success", "submit_verification_failure"],
            tool_call_limit=10,
        )
    )
    register_definition(
        AgentDefinition(
            name="evaluator",
            description="test evaluator",
            agent_kind=AgentKind.EVALUATOR,
            context_recipe="evaluator",
            terminals=["submit_evaluation"],
            tool_call_limit=10,
        )
    )
    yield


@pytest.fixture
def composer(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    context_packet_store,
    register_test_agents,
) -> AgentEntryComposer:
    deps = ContextEngineDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        context_packet_store=context_packet_store,
    )
    return AgentEntryComposer.default(ContextEngine(deps))

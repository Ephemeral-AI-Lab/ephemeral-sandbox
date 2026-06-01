"""Shared fixtures for task_center tests: in-memory SQLite DB + stores."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from agents import (
    AgentDefinition,
    AgentRole,
    list_definitions,
    register_definition,
    unregister_definition,
)
from db.base import Base
import db.models  # noqa: F401  - populates Base.metadata
from db.models.request import RequestRecord
from db.stores.workflow_store import WorkflowStore
from db.stores.attempt_store import AttemptStore
from db.stores.task_store import TaskStore
from db.stores.iteration_store import IterationStore
from workflow.agent_launch.composer import AgentEntryComposer
from workflow.context_engine.engine import ContextEngine, ContextEngineDeps


@pytest.fixture
def session_factory():
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    # Seed parent request for FK satisfaction.
    with sf() as s:
        s.add(
            RequestRecord(
                id="run1",
                cwd="/tmp",
                sandbox_id=None,
                request_prompt="prompt",
                status="running",
                created_at=datetime.now(UTC),
                updated_at=datetime.now(UTC),
            )
        )
        s.commit()
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
def task_store(session_factory) -> TaskStore:
    store = TaskStore()
    store.initialize(session_factory)
    return store


@pytest.fixture
def task_center_run_id() -> str:
    return "run1"


# ---------------------------------------------------------------------------
# Composer fixture for harness-attempt lifecycle tests
# ---------------------------------------------------------------------------
#
# Production paths (orchestrator + stage advancer + entry bootstrap) require an
# ``AgentEntryComposer`` on ``AttemptDeps``. Lifecycle tests that exercise
# planner/generator/reducer launches need (a) a composer wired into the
# runtime, (b) registered context recipes, and (c) minimal test agent
# definitions so the launch composer can look up a target agent.
#
# Tests opt in by depending on the ``composer`` fixture below.


@pytest.fixture
def isolated_agent_registries():
    """Save + restore agent registries for test isolation."""
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
def register_test_agents(request):
    """Register the bare-minimum agents needed by lifecycle tests.

    Provides ``planner``, ``executor``, ``generator``, ``reducer`` definitions
    each declaring the context role used by launch routing. Tests that need a
    different shape can register their own definitions on top; agent names are
    unique per test thanks to ``isolated_agent_registries`` cleanup.
    """
    request.getfixturevalue("isolated_agent_registries")
    register_definition(
        AgentDefinition(
            name="planner",
            description="test planner",
            role=AgentRole.PLANNER,
            context_recipe="planner",
            terminals=["submit_planner_outcome"],
            tool_call_limit=10,
        )
    )
    register_definition(
        AgentDefinition(
            name="executor",
            description="test executor",
            tool_call_limit=10,
            role=AgentRole.GENERATOR,
            context_recipe="generator",
            allowed_tools=["delegate_workflow", "check_workflow_status", "cancel_workflow"],
            terminals=["submit_generator_outcome"],
        )
    )
    register_definition(
        AgentDefinition(
            name="generator",
            description="test generator",
            role=AgentRole.GENERATOR,
            context_recipe="generator",
            terminals=["submit_generator_outcome"],
            tool_call_limit=10,
        )
    )
    register_definition(
        AgentDefinition(
            name="reducer",
            description="test reducer",
            role=AgentRole.REDUCER,
            context_recipe="reducer",
            terminals=["submit_reducer_outcome"],
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
    request,
) -> AgentEntryComposer:
    """Real AgentEntryComposer wired against the in-memory stores."""
    request.getfixturevalue("register_test_agents")
    deps = ContextEngineDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )
    return AgentEntryComposer.default(ContextEngine(deps))

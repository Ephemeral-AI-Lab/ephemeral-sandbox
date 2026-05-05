"""Phase 04 ``/api/db/task-center-runs/{id}/graph`` route tests."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from db.base import Base
import db.models  # noqa: F401  - populates Base.metadata
from db.models.task_center import TaskCenterRequestRecord, TaskCenterRunRecord
from db.stores.agent_run_store import AgentRunStore
from db.stores.complex_task_request_store import ComplexTaskRequestStore
from db.stores.harness_graph_store import HarnessGraphStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.task_segment_store import TaskSegmentStore
from server.routers.persistence import create_persistence_router
from task_center.mission.mission import ComplexTaskRequestStatus
from task_center.attempt import HarnessGraphStatus
from task_center.episode.episode import (
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)
from task_center.task import HarnessTaskRole, HarnessTaskStatus


@pytest.fixture
def stores():
    engine = create_engine(
        "sqlite:///:memory:",
        connect_args={"check_same_thread": False},
        poolclass=StaticPool,
    )
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    with sf() as s:
        s.add(
            TaskCenterRequestRecord(
                id="req1",
                cwd="/tmp",
                sandbox_id=None,
                request_prompt="prompt",
                created_at=datetime.now(UTC),
                updated_at=datetime.now(UTC),
            )
        )
        s.add(
            TaskCenterRunRecord(
                id="run1",
                request_id="req1",
                status="running",
                started_at=datetime.now(UTC),
            )
        )
        s.commit()
    task_center = TaskCenterStore()
    task_center.initialize(sf)
    agent_run = AgentRunStore()
    agent_run.initialize(sf)
    request_store = ComplexTaskRequestStore()
    request_store.initialize(sf)
    segment_store = TaskSegmentStore()
    segment_store.initialize(sf)
    graph_store = HarnessGraphStore()
    graph_store.initialize(sf)
    yield (task_center, agent_run, request_store, segment_store, graph_store)
    engine.dispose()


def _client(stores) -> TestClient:
    app = FastAPI()
    app.include_router(create_persistence_router(*stores))
    return TestClient(app)


def test_graph_route_walks_request_segment_graph_schema(stores):
    task_center, _, request_store, segment_store, graph_store = stores
    request = request_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="executor-1",
        goal="solve",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="solve",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment.id)
    graph = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, graph.id)
    task_center.upsert_task(
        task_id="task-1",
        task_center_run_id="run1",
        role=HarnessTaskRole.GENERATOR.value,
        agent_name="executor",
        task_input="do work",
        status=HarnessTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_harness_graph_id=graph.id,
    )

    response = _client(stores).get("/api/db/task-center-runs/run1/graph")
    assert response.status_code == 200
    body = response.json()

    assert "complex_task_requests" in body
    assert "harness_graphs_index" in body
    [r] = body["complex_task_requests"]
    assert r["id"] == request.id
    assert r["status"] == ComplexTaskRequestStatus.OPEN.value
    [s] = r["task_segments"]
    assert s["id"] == segment.id
    assert s["status"] == TaskSegmentStatus.OPEN.value
    [g] = s["harness_graphs"]
    assert g["id"] == graph.id
    assert g["status"] == HarnessGraphStatus.RUNNING.value
    assert {"task-1"} == {t["id"] for t in g["tasks"]}
    [idx] = body["harness_graphs_index"]
    assert idx == {
        "harness_graph_id": graph.id,
        "complex_task_request_id": request.id,
        "task_segment_id": segment.id,
    }


def test_graph_route_orders_by_sequence_no(stores):
    _, _, request_store, segment_store, graph_store = stores
    request = request_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="executor-1",
        goal="solve",
    )
    segment1 = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="seg1 goal",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment1.id)
    segment_store.set_continuation_goal(segment1.id, "go on")
    segment_store.set_status(
        segment1.id, status=TaskSegmentStatus.SUCCEEDED, closed_at=datetime.now(UTC)
    )
    segment2 = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=2,
        creation_reason=TaskSegmentCreationReason.PARTIAL_CONTINUATION,
        goal="go on",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment2.id)
    g1 = graph_store.insert(task_segment_id=segment1.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment1.id, g1.id)
    g2 = graph_store.insert(task_segment_id=segment2.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment2.id, g2.id)

    response = _client(stores).get("/api/db/task-center-runs/run1/graph")
    body = response.json()
    [r] = body["complex_task_requests"]
    seqs = [s["sequence_no"] for s in r["task_segments"]]
    assert seqs == [1, 2]


def test_graph_route_returns_503_when_stores_unready(stores):
    """Persistence stores must report 503 when not initialised."""
    task_center, agent_run, _, _, _ = stores
    # Use uninitialised graph stores to simulate not-ready state.
    request_store = ComplexTaskRequestStore()
    segment_store = TaskSegmentStore()
    graph_store = HarnessGraphStore()
    app = FastAPI()
    app.include_router(
        create_persistence_router(
            task_center, agent_run, request_store, segment_store, graph_store
        )
    )
    client = TestClient(app)
    response = client.get("/api/db/task-center-runs/run1/graph")
    assert response.status_code == 503


def test_graph_route_includes_retry_graphs_in_segment(stores):
    _, _, request_store, segment_store, graph_store = stores
    request = request_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="executor-1",
        goal="solve",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="solve",
        attempt_budget=3,
    )
    request_store.append_segment_id(request.id, segment.id)
    g1 = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, g1.id)
    g2 = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=2)
    segment_store.append_graph_id(segment.id, g2.id)

    response = _client(stores).get("/api/db/task-center-runs/run1/graph")
    body = response.json()
    [r] = body["complex_task_requests"]
    [s] = r["task_segments"]
    seqs = [g["graph_sequence_no"] for g in s["harness_graphs"]]
    assert seqs == [1, 2]

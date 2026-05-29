"""Unit tests for the SWE-EVO live e2e AuditRecorder.

Exercises the 4 ORM commit listeners (Workflow/Iteration/Attempt/Task) plus the
agent_run_id -> task_id mapping listener, the per-Task message recorder
gating by primary role, and the run.json/metrics.json writers.
"""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Iterator

import pytest
from sqlalchemy import Engine, create_engine
from sqlalchemy.orm import Session, sessionmaker

import db.models  # noqa: F401 - populate Base.metadata
from db.base import Base
from db.models.agent_run import AgentRunRecord
from db.models.task_center import (
    TaskCenterRequestRecord,
    TaskCenterRunRecord,
    TaskCenterTaskRecord,
)
from db.stores.attempt_store import AttemptStore
from db.stores.iteration_store import IterationStore
from db.stores.workflow_store import WorkflowStore
from db.stores.task_center_store import TaskCenterStore
from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.audit.events import Event, EventType
from task_center_runner.audit.node_id import NodeId
from task_center_runner.audit.recorder import AuditRecorder
from task_center import (
    IterationCreationReason,
    WorkflowStatus,
)


_RUN_ID = "run-abc"
_REQUEST_ID = "req-1"


@dataclass(slots=True)
class _TestStoreBundle:
    engine: Engine
    session_factory: sessionmaker[Session]
    task_store: TaskCenterStore
    workflow_store: WorkflowStore
    iteration_store: IterationStore
    attempt_store: AttemptStore

    def close(self) -> None:
        self.engine.dispose()


@pytest.fixture
def stores() -> Iterator[_TestStoreBundle]:
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    session_factory = sessionmaker(
        bind=engine,
        autoflush=False,
        expire_on_commit=False,
    )
    bundle = _TestStoreBundle(
        engine=engine,
        session_factory=session_factory,
        task_store=TaskCenterStore(),
        workflow_store=WorkflowStore(),
        iteration_store=IterationStore(),
        attempt_store=AttemptStore(),
    )
    for store in (
        bundle.task_store,
        bundle.workflow_store,
        bundle.iteration_store,
        bundle.attempt_store,
    ):
        store.initialize(session_factory)
    try:
        yield bundle
    finally:
        bundle.close()


def _seed_run(bundle: _TestStoreBundle, run_id: str = _RUN_ID) -> None:
    sf = bundle.session_factory
    with sf() as db:
        now = datetime.now(UTC)
        db.add(
            TaskCenterRequestRecord(
                id=_REQUEST_ID,
                cwd="/testbed",
                sandbox_id="sbx-1",
                request_prompt="goal",
                created_at=now,
                updated_at=now,
            )
        )
        db.add(
            TaskCenterRunRecord(
                id=run_id,
                request_id=_REQUEST_ID,
                status="running",
                started_at=now,
            )
        )
        db.commit()


def _read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def _read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def _make_recorder(
    tmp_path: Path,
    *,
    run_id: str = _RUN_ID,
    bus: AuditEventBus | None = None,
) -> AuditRecorder:
    return AuditRecorder(
        run_dir=tmp_path / "run",
        task_center_run_id=run_id,
        bus=bus,
        scenario_name="correctness_testing",
        instance_id="dask__dask_2023.3.2_2023.4.0",
        sandbox_id="sbx-1",
    )


def test_goal_insert_writes_latest_snapshot(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        goal = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            requested_by_task_id="parent_task_1",
            goal="solve the problem",
        )
    finally:
        recorder.dispose()

    workflow_dir = recorder.run_dir / f"workflow_01_{goal.id}"
    snapshot = workflow_dir / "workflow.json"
    assert snapshot.exists()
    row = _read_json(snapshot)
    assert row["id"] == goal.id
    assert row["status"] == "open"
    assert "context" not in row
    assert "summary" not in row
    assert not (workflow_dir / "workflow.jsonl").exists()


def test_goal_update_overwrites_latest_snapshot(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        goal = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            requested_by_task_id="parent_task_1",
            goal="solve the problem",
        )
        stores.workflow_store.set_status(
            goal.id,
            status=WorkflowStatus.SUCCEEDED,
            final_outcome={"ok": True},
            closed_at=datetime.now(UTC),
        )
    finally:
        recorder.dispose()

    snapshot = recorder.run_dir / f"workflow_01_{goal.id}" / "workflow.json"
    row = _read_json(snapshot)
    assert row["status"] == "succeeded"
    assert row["final_outcome"] == {"ok": True}


def test_iteration_and_attempt_listeners(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        goal = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            requested_by_task_id="parent_task_1",
            goal="solve the problem",
        )
        iteration = stores.iteration_store.insert(
            workflow_id=goal.id,
            sequence_no=1,
            creation_reason=IterationCreationReason.INITIAL,
            goal="ep goal",
            attempt_budget=3,
        )
        attempt = stores.attempt_store.insert(
            iteration_id=iteration.id,
            attempt_sequence_no=1,
        )
    finally:
        recorder.dispose()

    workflow_dir = recorder.run_dir / f"workflow_01_{goal.id}"
    iteration_dir = workflow_dir / f"iteration_01_{iteration.id}"
    attempt_dir = iteration_dir / f"attempt_01_{attempt.id}"
    iteration_row = _read_json(iteration_dir / "iteration.json")
    attempt_row = _read_json(attempt_dir / "attempt.json")
    assert iteration_row["id"] == iteration.id
    assert "context" not in iteration_row
    assert "summary" not in iteration_row
    assert attempt_row["id"] == attempt.id
    assert "context" not in attempt_row
    assert "summary" not in attempt_row
    assert not (iteration_dir / "iteration.jsonl").exists()
    assert not (attempt_dir / "attempt.jsonl").exists()


def _insert_task(
    bundle: _TestStoreBundle,
    *,
    task_id: str,
    role: str,
    run_id: str = _RUN_ID,
    task_center_attempt_id: str | None = None,
    agent_name: str | None = None,
) -> None:
    sf = bundle.session_factory
    with sf() as db:
        now = datetime.now(UTC)
        db.add(
            TaskCenterTaskRecord(
                id=task_id,
                task_center_run_id=run_id,
                role=role,
                agent_name=agent_name,
                context_message="input",
                status="pending",
                summaries=[],
                needs=[],
                task_center_attempt_id=task_center_attempt_id,
                context_packet_id=None,
                created_at=now,
                updated_at=now,
            )
        )
        db.commit()


def test_task_dir_placement_per_role(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        goal = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            goal="goal",
        )
        iteration = stores.iteration_store.insert(
            workflow_id=goal.id,
            sequence_no=1,
            creation_reason=IterationCreationReason.INITIAL,
            goal="ep",
            attempt_budget=3,
        )
        attempt = stores.attempt_store.insert(
            iteration_id=iteration.id,
            attempt_sequence_no=1,
        )

        _insert_task(
            stores,
            task_id="task_planner",
            role="planner",
            task_center_attempt_id=attempt.id,
        )
        _insert_task(
            stores,
            task_id="task_executor",
            role="executor",
            task_center_attempt_id=attempt.id,
        )
        _insert_task(
            stores,
            task_id="task_evaluator",
            role="evaluator",
            task_center_attempt_id=attempt.id,
        )
    finally:
        recorder.dispose()

    attempt_dir = (
        recorder.run_dir
        / f"workflow_01_{goal.id}"
        / f"iteration_01_{iteration.id}"
        / f"attempt_01_{attempt.id}"
    )
    assert (attempt_dir / "01_planner_task_planner" / "task.json").exists()
    assert (attempt_dir / "02_executor_task_executor" / "task.json").exists()
    assert (attempt_dir / "03_evaluator_task_evaluator" / "task.json").exists()


def test_helper_role_filtered(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        _insert_task(stores, task_id="helper_1", role="helper")
    finally:
        recorder.dispose()

    helper_dirs = list(recorder.run_dir.glob("*helper_1*"))
    assert helper_dirs == []


def test_generator_verifier_task_uses_verifier_dir_and_message_recorder(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        goal = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            requested_by_task_id="parent_task_1",
            goal="goal",
        )
        iteration = stores.iteration_store.insert(
            workflow_id=goal.id,
            sequence_no=1,
            creation_reason=IterationCreationReason.INITIAL,
            goal="ep",
            attempt_budget=3,
        )
        attempt = stores.attempt_store.insert(
            iteration_id=iteration.id,
            attempt_sequence_no=1,
        )
        _insert_task(
            stores,
            task_id="task_verifier",
            role="generator",
            agent_name="verifier",
            task_center_attempt_id=attempt.id,
        )
    finally:
        recorder.dispose()

    verifier_dir = (
        recorder.run_dir
        / f"workflow_01_{goal.id}"
        / f"iteration_01_{iteration.id}"
        / f"attempt_01_{attempt.id}"
        / "01_verifier_task_verifier"
    )
    assert (verifier_dir / "task.json").exists()
    assert recorder.message_recorder_for_task("task_verifier") is not None


def test_sandbox_events_are_mirrored_to_run_jsonl(tmp_path: Path) -> None:
    bus = AuditEventBus()
    recorder = _make_recorder(tmp_path, bus=bus)
    recorder.start()
    try:
        bus.publish(
            Event(
                type=EventType.SANDBOX_OCC_CHANGES_COMMITTED,
                node=NodeId(task_center_run_id=_RUN_ID, tool_name="write_file"),
                payload={"status": "committed", "changed_paths": ["a.txt"]},
                correlation_id="corr-1",
            )
        )
        bus.publish(
            Event(
                type=EventType.EXECUTOR_SUCCESS,
                node=NodeId(task_center_run_id=_RUN_ID, agent_name="executor"),
                payload={"checkpoint": "done"},
            )
        )
    finally:
        recorder.dispose()

    rows = _read_jsonl(recorder.run_dir / "sandbox_events.jsonl")
    assert len(rows) == 1
    assert rows[0]["event_type"] == "sandbox_occ_changes_committed"
    assert rows[0]["node"]["task_center_run_id"] == _RUN_ID
    assert rows[0]["node"]["tool_name"] == "write_file"
    assert rows[0]["payload"] == {
        "status": "committed",
        "changed_paths": ["a.txt"],
    }
    assert rows[0]["correlation_id"] == "corr-1"


def test_dispose_unregisters_listeners(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        m1 = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            requested_by_task_id="parent_task_1",
            goal="g1",
        )
    finally:
        recorder.dispose()

    m2 = stores.workflow_store.insert(
        task_center_run_id=_RUN_ID,
        requested_by_task_id="parent_task_2",
        goal="g2",
    )

    assert (recorder.run_dir / f"workflow_01_{m1.id}").exists()
    assert not (recorder.run_dir / f"workflow_02_{m2.id}").exists()


def test_run_json_and_metrics_json_written(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    run_json = recorder.run_dir / "run.json"
    assert run_json.exists()
    started = json.loads(run_json.read_text())
    assert started["status"] == "running"
    assert started["task_center_run_id"] == _RUN_ID

    recorder.dispose()
    finished = json.loads(run_json.read_text())
    assert finished["status"] == "finished"
    assert finished["finished_ts"] is not None

    metrics_json = recorder.run_dir / "metrics.json"
    assert metrics_json.exists()
    payload = json.loads(metrics_json.read_text())
    assert "per_tool" in payload


def test_write_performance_reports_produces_detailed_report(tmp_path: Path) -> None:
    bus = AuditEventBus()
    recorder = _make_recorder(tmp_path, bus=bus)
    started = datetime.now(UTC)
    recorder.start()
    try:
        node = NodeId(
            task_center_run_id=_RUN_ID,
            agent_name="executor",
            agent_run_id="agent-run-1",
            tool_name="write_file",
        )
        bus.publish(
            Event(
                type=EventType.TOOL_CALL_STARTED,
                node=node,
                payload={
                    "tool_name": "write_file",
                    "tool_use_id": "toolu_1",
                    "tool_input": {"file_path": "a.py", "content": "print(1)"},
                },
                ts=started,
            )
        )
        bus.publish(
            Event(
                type=EventType.TOOL_CALL_COMPLETED,
                node=node,
                payload={
                    "tool_name": "write_file",
                    "tool_use_id": "toolu_1",
                    "output": '{"ok": true}',
                    "is_error": False,
                    "metadata": {
                        "status": "ok",
                        "changed_paths": ["a.py"],
                        "timings": {
                            "api.write.occ_apply_s": 0.04,
                            "occ.commit.publish_layer_s": 0.01,
                        },
                    },
                },
                ts=started + timedelta(milliseconds=125),
            )
        )
        bus.publish(
            Event(
                type=EventType.SANDBOX_OCC_CHANGES_COMMITTED,
                node=node,
                payload={
                    "tool_name": "write_file",
                    "tool_use_id": "toolu_1",
                    "status": "ok",
                    "changed_paths": ["a.py"],
                    "timings": {
                        "api.write.occ_apply_s": 0.04,
                        "occ.commit.publish_layer_s": 0.01,
                    },
                },
            )
        )
        bus.publish(
            Event(
                type=EventType.SANDBOX_OVERLAY_EXECUTED,
                node=node,
                payload={
                    "tool_name": "shell",
                    "tool_use_id": "toolu_2",
                    "status": "ok",
                    "changed_paths": ["b.py"],
                    "timings": {
                        "command_exec.mount_workspace_s": 0.02,
                        "command_exec.run_command_s": 0.07,
                        "command_exec.capture_upperdir_s": 0.03,
                        "command_exec.total_s": 0.18,
                        "api.shell.overlay_s": 0.12,
                        "api.shell.total_s": 0.18,
                    },
                },
            )
        )
        bus.publish(
            Event(
                type=EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED,
                node=node,
                payload={
                    "tool_name": "write_file",
                    "tool_use_id": "toolu_3",
                    "status": "ok",
                    "changed_paths": [],
                    "timings": {
                        "layer_stack.auto_squash.total_s": 0.5,
                        "layer_stack.auto_squash.depth_before": 40,
                    },
                },
            )
        )
        bus.publish(
            Event(
                type=EventType.SANDBOX_RESOURCE_SNAPSHOT,
                node=node,
                payload={
                    "tool_name": "shell",
                    "tool_use_id": "toolu_4",
                    "status": "ok",
                    "changed_paths": [],
                    "timings": {
                        "resource.command_exec.upperdir_tree_bytes": 4096,
                        "resource.cgroup.memory_current_bytes": 123456,
                        "resource.cgroup.cpu_usage_usec": 500,
                        "resource.cgroup.io_wbytes": 1000,
                    },
                },
            )
        )
        bus.publish(
            Event(
                type=EventType.SANDBOX_RESOURCE_SNAPSHOT,
                node=node,
                payload={
                    "tool_name": "shell",
                    "tool_use_id": "toolu_5",
                    "status": "ok",
                    "changed_paths": [],
                    "timings": {
                        "resource.cgroup.cpu_usage_usec": 800,
                        "resource.cgroup.io_wbytes": 1500,
                    },
                },
            )
        )
        snapshot = recorder.metrics.performance_snapshot()
    finally:
        recorder.dispose()

    metrics_payload = _read_json(recorder.run_dir / "metrics.json")
    assert "samples" not in metrics_payload["per_tool"]["write_file"]
    assert not (recorder.run_dir / "performance_report.json").exists(), (
        "dispose() must no longer write the perf report; Phase 3 moved it to "
        "an async post-dispose task driven by the caller."
    )

    from task_center_runner.audit.performance_report import write_performance_reports

    write_performance_reports(recorder.run_dir, snapshot)

    report = _read_json(recorder.run_dir / "performance_report.json")
    # Phase 3 bumped the perf-report schema string; legacy ``tools`` /
    # ``hotspots`` / ``sandbox.families`` blocks remain populated for
    # back-compat (see ``performance_report._build_legacy_sandbox_report``).
    assert report["schema"] == "task_center_runner.performance_report.v3"
    assert report["totals"]["tool_calls_total"] == 1
    assert report["tools"]["per_tool"]["write_file"]["p95_ms"] == 125.0
    assert report["tools"]["per_tool"]["write_file"]["samples"][0][
        "changed_paths"
    ] == ["a.py"]
    assert report["sandbox"]["families"]["occ"]["event_count"] == 1
    assert report["sandbox"]["families"]["overlay"]["event_count"] == 1
    assert report["sandbox"]["families"]["layer_stack"]["event_count"] == 1
    assert report["sandbox"]["families"]["resource"]["event_count"] == 2
    assert report["sandbox"]["timing_keys"]["api.shell.overlay_s"]["total"] == 0.12
    assert report["sandbox"]["timing_keys"]["command_exec.mount_workspace_s"][
        "total"
    ] == 0.02
    assert report["sandbox"]["timing_keys"]["command_exec.run_command_s"][
        "total"
    ] == 0.07
    assert report["sandbox"]["timing_keys"]["command_exec.capture_upperdir_s"][
        "total"
    ] == 0.03
    assert report["sandbox"]["timing_keys"]["command_exec.total_s"]["total"] == 0.18
    assert report["sandbox"]["timing_keys"]["api.shell.total_s"]["total"] == 0.18
    assert report["sandbox"]["non_duration_observations"][
        "layer_stack.auto_squash.depth_before"
    ]["max"] == 40.0
    assert report["sandbox"]["resource_keys"][
        "resource.command_exec.upperdir_tree_bytes"
    ]["latest"] == 4096.0
    assert report["sandbox"]["resource_keys"]["resource.cgroup.cpu_usage_usec"][
        "source"
    ] == "run_delta"
    assert report["sandbox"]["resource_keys"]["resource.cgroup.cpu_usage_usec"][
        "latest"
    ] == 300.0
    assert report["sandbox"]["resource_keys"]["resource.cgroup.io_wbytes"][
        "latest"
    ] == 500.0
    assert report["hotspots"]["slowest_tool_calls"][0]["tool_use_id"] == "toolu_1"

    markdown = (recorder.run_dir / "performance_report.md").read_text(
        encoding="utf-8"
    )
    # V3 fixed §1-§13 layout — headers are stable per the phase-3 spec.
    assert "## 1. Summary" in markdown
    assert "## 2. Per-tool timing" in markdown
    assert "## 10. OS resource" in markdown
    # Tool names continue to surface in §2's per-tool table when their
    # ``tool_call.*`` events are pulled (mocks emit none here, but the
    # legacy ``tools`` block populates `performance_report.json` via
    # `tool_performance` so the v2 surface stays observable).
    assert report["tools"]["per_tool"]["write_file"]["count"] == 1


def test_agent_run_id_to_task_id_mapping(
    tmp_path: Path, stores: _TestStoreBundle
) -> None:
    """The 5th listener populates agent_run_id -> task_id."""
    _seed_run(stores)
    recorder = _make_recorder(tmp_path)
    recorder.start()
    try:
        goal = stores.workflow_store.insert(
            task_center_run_id=_RUN_ID,
            goal="goal",
        )
        iteration = stores.iteration_store.insert(
            workflow_id=goal.id,
            sequence_no=1,
            creation_reason=IterationCreationReason.INITIAL,
            goal="ep",
            attempt_budget=3,
        )
        attempt = stores.attempt_store.insert(
            iteration_id=iteration.id,
            attempt_sequence_no=1,
        )
        _insert_task(
            stores,
            task_id="task_planner",
            role="planner",
            agent_name="planner",
            task_center_attempt_id=attempt.id,
        )
        agent_run_id = str(uuid.uuid4())
        sf = stores.session_factory
        with sf() as db:
            db.add(
                AgentRunRecord(
                    id=agent_run_id,
                    task_id="task_planner",
                    agent_name="planner",
                    message_history=None,
                    terminal_tool_result=None,
                    token_count=0,
                    error=None,
                    created_at=datetime.now(UTC),
                )
            )
            db.commit()

        rec = recorder.message_recorder_for_agent_run(agent_run_id)
        assert rec is not None
        assert recorder.message_recorder_for_task("task_planner") is rec
    finally:
        recorder.dispose()

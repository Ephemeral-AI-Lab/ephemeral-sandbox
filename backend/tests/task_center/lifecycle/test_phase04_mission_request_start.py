"""Phase 04 mission request starter tests.

Covers happy path, startup failure rollback, and duplicate-open-request gating.
"""

from __future__ import annotations

import pytest

from task_center.mission.starter import (
    MissionRequestStarter,
    StartedMissionRequest,
)
from task_center.mission.mission import ComplexTaskRequestStatus
from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, HarnessGraphRuntime
from task_center.attempt import (
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import (
    TaskSegmentCreationReason,
    TaskSegmentStatus,
    )
from task_center.task import HarnessTaskRole, HarnessTaskStatus, planner_task_id


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailingLauncher:
    def launch(self, launch: AgentLaunch) -> None:
        del launch
        raise RuntimeError("delegated planner launch boom")


def _build_runtime(
    request_store, segment_store, graph_store, task_store, *, composer, launcher=None
) -> HarnessGraphRuntime:
    launcher = launcher or _FakeLauncher()
    registry = HarnessGraphOrchestratorRegistry()
    return HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        manager_registry=SegmentManagerRegistry(),
        composer=composer,
    )


def _seed_outer_generator_task(
    *,
    task_store,
    request_store,
    segment_store,
    graph_store,
    task_center_run_id: str,
) -> tuple[str, str]:
    """Seed an outer generator task whose graph is currently RUNNING."""
    outer_request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="root",
        goal="outer goal",
    )
    outer_segment = segment_store.insert(
        complex_task_request_id=outer_request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="outer goal",
        attempt_budget=2,
    )
    request_store.append_segment_id(outer_request.id, outer_segment.id)
    outer_graph = graph_store.insert(
        task_segment_id=outer_segment.id, graph_sequence_no=1
    )
    segment_store.append_graph_id(outer_segment.id, outer_graph.id)

    parent_task_id = "outer-generator-task"
    task_store.upsert_task(
        task_id=parent_task_id,
        task_center_run_id=task_center_run_id,
        role=HarnessTaskRole.GENERATOR.value,
        agent_name="executor",
        task_input="execute the outer task",
        status=HarnessTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_harness_graph_id=outer_graph.id,
        spawn_reason="harness_graph_generator",
    )
    return parent_task_id, outer_graph.id


def test_mission_start_creates_request_segment_graph_and_marks_parent_waiting(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        request_store, segment_store, graph_store, task_store, composer=composer
    )
    parent_task_id, parent_graph_id = _seed_outer_generator_task(
        task_store=task_store,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = MissionRequestStarter(runtime=runtime)

    result: StartedMissionRequest = coordinator.start(
        task_center_run_id=task_center_run_id,
        parent_task_id=parent_task_id,
        parent_harness_graph_id=parent_graph_id,
        goal="solve delegated task",
    )

    delegated_request = request_store.get(result.complex_task_request_id)
    initial_segment = segment_store.get(result.initial_segment_id)
    initial_graph = graph_store.get(result.initial_harness_graph_id)
    parent_task = task_store.get_task(parent_task_id)

    assert delegated_request is not None
    assert delegated_request.status == ComplexTaskRequestStatus.OPEN
    assert delegated_request.requested_by_task_id == parent_task_id
    assert delegated_request.goal == "solve delegated task"
    assert initial_segment is not None
    assert initial_segment.complex_task_request_id == delegated_request.id
    assert initial_graph is not None
    assert initial_graph.task_segment_id == initial_segment.id
    assert parent_task is not None
    assert parent_task["status"] == HarnessTaskStatus.WAITING_COMPLEX_TASK.value
    # Delegated orchestrator was started.
    assert runtime.orchestrator_registry.get(initial_graph.id) is not None


def test_mission_start_startup_failure_leaves_parent_running(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        request_store, segment_store, graph_store, task_store, composer=composer
    )
    parent_task_id, parent_graph_id = _seed_outer_generator_task(
        task_store=task_store,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_center_run_id=task_center_run_id,
    )

    def _failing_factory(graph, on_graph_closed):
        del graph, on_graph_closed
        raise RuntimeError("delegated startup boom")

    coordinator = MissionRequestStarter(runtime=runtime)
    # Patch the factory used by the coordinator's handler builder.
    original = MissionRequestStarter._build_handler

    def _patched_build_handler(self):
        handler = original(self)
        handler._orchestrator_factory = _failing_factory  # type: ignore[attr-defined]
        return handler

    MissionRequestStarter._build_handler = _patched_build_handler  # type: ignore[assignment]
    try:
        with pytest.raises(RuntimeError):
            coordinator.start(
                task_center_run_id=task_center_run_id,
                parent_task_id=parent_task_id,
                parent_harness_graph_id=parent_graph_id,
                goal="delegated",
            )
    finally:
        MissionRequestStarter._build_handler = original  # type: ignore[assignment]

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == HarnessTaskStatus.RUNNING.value
    # The compensation path must mark the request and segment cancelled.
    open_requests = [
        r
        for r in request_store.list_for_executor_task(parent_task_id)
        if r.is_open
    ]
    assert open_requests == []
    cancelled = [
        r
        for r in request_store.list_for_executor_task(parent_task_id)
        if r.status == ComplexTaskRequestStatus.CANCELLED
    ]
    assert len(cancelled) == 1
    assert cancelled[0].requested_by_task_id == parent_task_id
    cancelled_segment = segment_store.list_for_request(cancelled[0].id)
    assert len(cancelled_segment) == 1
    assert cancelled_segment[0].status == TaskSegmentStatus.CANCELLED
    assert runtime.manager_registry is not None
    assert runtime.manager_registry.get(cancelled_segment[0].id) is None


def test_mission_start_startup_failure_closes_started_graph_and_deregisters_orchestrator(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        request_store,
        segment_store,
        graph_store,
        task_store,
        launcher=_FailingLauncher(),
        composer=composer,
    )
    parent_task_id, parent_graph_id = _seed_outer_generator_task(
        task_store=task_store,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = MissionRequestStarter(runtime=runtime)

    with pytest.raises(RuntimeError):
        coordinator.start(
            task_center_run_id=task_center_run_id,
            parent_task_id=parent_task_id,
            parent_harness_graph_id=parent_graph_id,
            goal="delegated",
        )

    [cancelled_request] = [
        r
        for r in request_store.list_for_executor_task(parent_task_id)
        if r.status == ComplexTaskRequestStatus.CANCELLED
    ]
    [cancelled_segment] = segment_store.list_for_request(cancelled_request.id)
    [failed_graph] = graph_store.list_for_segment(cancelled_segment.id)
    assert failed_graph.status == HarnessGraphStatus.FAILED
    assert failed_graph.fail_reason == HarnessGraphFailReason.STARTUP_FAILED
    assert runtime.orchestrator_registry.get(failed_graph.id) is None
    assert runtime.manager_registry is not None
    assert runtime.manager_registry.get(cancelled_segment.id) is None
    planner_task = task_store.get_task(planner_task_id(failed_graph.id))
    assert planner_task is not None
    assert planner_task["status"] == HarnessTaskStatus.FAILED.value


def test_mission_start_rejects_second_open_child_request_for_same_executor(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        request_store, segment_store, graph_store, task_store, composer=composer
    )
    parent_task_id, parent_graph_id = _seed_outer_generator_task(
        task_store=task_store,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = MissionRequestStarter(runtime=runtime)
    coordinator.start(
        task_center_run_id=task_center_run_id,
        parent_task_id=parent_task_id,
        parent_harness_graph_id=parent_graph_id,
        goal="first delegation",
    )

    # Restore the parent to running so the second call passes the running gate
    # but is rejected by the duplicate-open-request check.
    task_store.set_task_status(
        parent_task_id,
        status=HarnessTaskStatus.RUNNING.value,
    )

    with pytest.raises(GraphInvariantViolation) as exc:
        coordinator.start(
            task_center_run_id=task_center_run_id,
            parent_task_id=parent_task_id,
            parent_harness_graph_id=parent_graph_id,
            goal="second delegation",
        )
    assert "open complex-task request" in str(exc.value)


def test_mission_start_rejects_non_running_parent(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        request_store, segment_store, graph_store, task_store, composer=composer
    )
    parent_task_id, parent_graph_id = _seed_outer_generator_task(
        task_store=task_store,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_center_run_id=task_center_run_id,
    )
    task_store.set_task_status(
        parent_task_id, status=HarnessTaskStatus.DONE.value
    )

    coordinator = MissionRequestStarter(runtime=runtime)
    with pytest.raises(GraphInvariantViolation) as exc:
        coordinator.start(
            task_center_run_id=task_center_run_id,
            parent_task_id=parent_task_id,
            parent_harness_graph_id=parent_graph_id,
            goal="delegated",
        )
    assert "not running" in str(exc.value)


def test_mission_start_accepts_entry_mode_caller_with_no_parent_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    """Entry-mode caller has ``parent_harness_graph_id=None``.

    The mission starter must accept that and route the parent-waiting
    transition through the runtime's :class:`EntryTaskController` so the
    controller stays the single owner of entry-task state transitions.
    """
    from task_center.mission.handler import ComplexTaskRequestHandler
    from task_center.config import HarnessLifecycleConfig
    from task_center.entry_task_controller import EntryTaskController

    # Seed the entry-mode caller: an entry task with task_center_harness_graph_id=None.
    entry_task_id = "entry-task-id"
    entry_request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id=entry_task_id,
        goal="entry goal",
    )
    handler = ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=SegmentManagerRegistry(),
        config=HarnessLifecycleConfig(),
    )
    entry_segment, _ = handler.create_initial_episode_with_manager(
        complex_task_request_id=entry_request.id
    )
    task_store.upsert_task(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        role=HarnessTaskRole.GENERATOR.value,
        agent_name="entry_executor",
        task_input="entry goal",
        status=HarnessTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_harness_graph_id=None,
        spawn_reason="entry_executor",
    )
    controller = EntryTaskController(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        complex_task_request_id=entry_request.id,
        task_segment_id=entry_segment.id,
        task_store=task_store,
        segment_store=segment_store,
        request_handler=handler,
        manager_registry=handler._manager_registry,  # type: ignore[attr-defined]
    )
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=HarnessGraphOrchestratorRegistry(),
        manager_registry=handler._manager_registry,  # type: ignore[attr-defined]
        composer=composer,
        entry_task_controller=controller,
    )

    coordinator = MissionRequestStarter(runtime=runtime)
    result: StartedMissionRequest = coordinator.start(
        task_center_run_id=task_center_run_id,
        parent_task_id=entry_task_id,
        parent_harness_graph_id=None,
        goal="solve delegated work",
    )

    # Entry task is now WAITING_COMPLEX_TASK via the controller.
    entry_task = task_store.get_task(entry_task_id)
    assert entry_task is not None
    assert entry_task["status"] == HarnessTaskStatus.WAITING_COMPLEX_TASK.value
    # Result carries None for parent_harness_graph_id (entry mode).
    assert result.parent_harness_graph_id is None
    # Delegated request + segment + graph were all created and started.
    delegated_request = request_store.get(result.complex_task_request_id)
    delegated_segment = segment_store.get(result.initial_segment_id)
    delegated_graph = graph_store.get(result.initial_harness_graph_id)
    assert delegated_request is not None
    assert delegated_request.status == ComplexTaskRequestStatus.OPEN
    assert delegated_segment is not None
    assert delegated_graph is not None
    assert runtime.orchestrator_registry.get(delegated_graph.id) is not None

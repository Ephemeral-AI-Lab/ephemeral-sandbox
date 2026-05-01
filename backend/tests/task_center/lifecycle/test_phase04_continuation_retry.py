"""Phase 04 end-to-end continuation and retry tests.

Drives the full coordinator → handler → manager → orchestrator pipeline so
that retry, continuation, and final close-report routing are exercised
together. The parent task must remain in ``waiting_complex_task`` until the
delegated request closes terminally.
"""

from __future__ import annotations

from task_center.complex_task.handoff import ComplexTaskHandoffCoordinator
from task_center.complex_task.request import ComplexTaskRequestStatus
from task_center.harness_graph.orchestrator import HarnessGraphOrchestrator
from task_center.harness_graph.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.harness_graph.graph import (
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.harness_graph.runtime import HarnessAgentLaunch, HarnessGraphRuntime
from task_center.segment.registry import SegmentManagerRegistry
from task_center.segment.segment import (
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    HarnessTaskStatus,
    PlannedGeneratorTask,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[HarnessAgentLaunch] = []

    def launch(self, launch: HarnessAgentLaunch) -> None:
        self.launches.append(launch)


class _FailOnLaunchNumber(_FakeLauncher):
    def __init__(self, fail_on: int) -> None:
        super().__init__()
        self._fail_on = fail_on

    def launch(self, launch: HarnessAgentLaunch) -> None:
        super().launch(launch)
        if len(self.launches) == self._fail_on:
            raise RuntimeError("planned launch failure")


def _build_runtime(
    request_store, segment_store, graph_store, task_store, launcher=None
) -> HarnessGraphRuntime:
    return HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher or _FakeLauncher(),
        orchestrator_registry=HarnessGraphOrchestratorRegistry(),
        manager_registry=SegmentManagerRegistry(),
    )


def _seed_outer_running_generator(
    *,
    runtime: HarnessGraphRuntime,
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id: str,
) -> tuple[str, str]:
    """Seed an outer parent graph + a single running generator task on it."""
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
    outer_orchestrator = HarnessGraphOrchestrator(
        harness_graph=outer_graph,
        on_graph_closed=lambda graph_id: None,
        runtime=runtime,
    )
    runtime.orchestrator_registry.register(outer_orchestrator)
    outer_orchestrator.start()
    outer_orchestrator.apply_plan_submission(
        PlannerSubmission(
            graph_id=outer_graph.id,
            planner_task_id=planner_task_id(outer_graph.id),
            kind="full",
            task_specification="outer spec",
            evaluation_criteria=("outer ok",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="outer",
                    agent_name="executor",
                    deps=(),
                    task_spec="execute outer",
                ),
            ),
            continuation_goal=None,
            summary="outer plan",
        )
    )
    parent_task_id = generator_task_id(outer_graph.id, "outer")
    return parent_task_id, outer_graph.id


def _drive_delegated_graph_to_pass(
    *,
    runtime: HarnessGraphRuntime,
    delegated_graph_id: str,
    continuation_goal: str | None,
) -> None:
    """Plan, execute, and pass the delegated graph."""
    delegated = runtime.orchestrator_registry.get_or_raise(delegated_graph_id)
    if continuation_goal is None:
        delegated.apply_plan_submission(
            PlannerSubmission(
                graph_id=delegated_graph_id,
                planner_task_id=planner_task_id(delegated_graph_id),
                kind="full",
                task_specification="delegated spec",
                evaluation_criteria=("delegated ok",),
                tasks=(
                    PlannedGeneratorTask(
                        local_id="d",
                        agent_name="executor",
                        deps=(),
                        task_spec="do delegated",
                    ),
                ),
                continuation_goal=None,
                summary="delegated plan",
            )
        )
    else:
        delegated.apply_plan_submission(
            PlannerSubmission(
                graph_id=delegated_graph_id,
                planner_task_id=planner_task_id(delegated_graph_id),
                kind="partial",
                task_specification="delegated spec",
                evaluation_criteria=("delegated ok",),
                tasks=(
                    PlannedGeneratorTask(
                        local_id="d",
                        agent_name="executor",
                        deps=(),
                        task_spec="do delegated",
                    ),
                ),
                continuation_goal=continuation_goal,
                summary="delegated plan",
            )
        )
    delegated.apply_generator_submission(
        GeneratorSubmission(
            graph_id=delegated_graph_id,
            task_id=generator_task_id(delegated_graph_id, "d"),
            outcome="success",
            summary="generator ok",
            payload={},
        )
    )
    delegated.apply_evaluator_submission(
        EvaluatorSubmission(
            graph_id=delegated_graph_id,
            task_id=evaluator_task_id(delegated_graph_id),
            outcome="success",
            summary="evaluator ok",
            payload={},
        )
    )


def _drive_delegated_graph_to_fail(
    *,
    runtime: HarnessGraphRuntime,
    delegated_graph_id: str,
) -> None:
    delegated = runtime.orchestrator_registry.get_or_raise(delegated_graph_id)
    delegated.apply_plan_submission(
        PlannerSubmission(
            graph_id=delegated_graph_id,
            planner_task_id=planner_task_id(delegated_graph_id),
            kind="full",
            task_specification="delegated spec",
            evaluation_criteria=("delegated ok",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="d",
                    agent_name="executor",
                    deps=(),
                    task_spec="do delegated",
                ),
            ),
            continuation_goal=None,
            summary="delegated plan",
        )
    )
    delegated.apply_generator_submission(
        GeneratorSubmission(
            graph_id=delegated_graph_id,
            task_id=generator_task_id(delegated_graph_id, "d"),
            outcome="failure",
            summary="generator failed",
            payload={},
        )
    )


def test_delegated_continuation_waits_until_final_segment(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    runtime = _build_runtime(
        request_store, segment_store, graph_store, task_store
    )
    parent_task_id, parent_graph_id = _seed_outer_running_generator(
        runtime=runtime,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = ComplexTaskHandoffCoordinator(runtime=runtime)
    handoff = coordinator.start(
        task_center_run_id=task_center_run_id,
        parent_task_id=parent_task_id,
        parent_harness_graph_id=parent_graph_id,
        goal="delegated continuation",
    )

    segment1_initial_graph_id = handoff.initial_harness_graph_id

    # Segment 1 passes with continuation goal — parent must remain WAITING.
    _drive_delegated_graph_to_pass(
        runtime=runtime,
        delegated_graph_id=segment1_initial_graph_id,
        continuation_goal="continue work",
    )
    parent_after_segment1 = task_store.get_task(parent_task_id)
    assert parent_after_segment1 is not None
    assert (
        parent_after_segment1["status"]
        == HarnessTaskStatus.WAITING_COMPLEX_TASK.value
    )
    delegated_request_after_segment1 = request_store.get(
        handoff.complex_task_request_id
    )
    assert delegated_request_after_segment1 is not None
    assert delegated_request_after_segment1.status == ComplexTaskRequestStatus.OPEN
    assert len(delegated_request_after_segment1.task_segment_ids) == 2

    # Segment 2 starts from the new continuation graph the handler created.
    segment2_id = delegated_request_after_segment1.task_segment_ids[1]
    segment2 = segment_store.get(segment2_id)
    assert segment2 is not None
    assert segment2.goal == "continue work"
    segment2_initial_graph_id = segment2.harness_graph_ids[0]
    # Drive segment 2 to terminal success.
    _drive_delegated_graph_to_pass(
        runtime=runtime,
        delegated_graph_id=segment2_initial_graph_id,
        continuation_goal=None,
    )

    parent_final = task_store.get_task(parent_task_id)
    delegated_final = request_store.get(handoff.complex_task_request_id)
    segment2_final = segment_store.get(segment2_id)
    assert parent_final is not None
    assert parent_final["status"] == HarnessTaskStatus.DONE.value
    assert delegated_final is not None
    assert delegated_final.status == ComplexTaskRequestStatus.SUCCEEDED
    assert segment2_final is not None
    assert segment2_final.status == TaskSegmentStatus.SUCCEEDED


def test_continuation_startup_failure_reports_continuation_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    launcher = _FailOnLaunchNumber(fail_on=6)
    runtime = _build_runtime(
        request_store,
        segment_store,
        graph_store,
        task_store,
        launcher=launcher,
    )
    parent_task_id, parent_graph_id = _seed_outer_running_generator(
        runtime=runtime,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = ComplexTaskHandoffCoordinator(runtime=runtime)
    handoff = coordinator.start(
        task_center_run_id=task_center_run_id,
        parent_task_id=parent_task_id,
        parent_harness_graph_id=parent_graph_id,
        goal="delegated continuation",
    )

    _drive_delegated_graph_to_pass(
        runtime=runtime,
        delegated_graph_id=handoff.initial_harness_graph_id,
        continuation_goal="continue work",
    )

    request = request_store.get(handoff.complex_task_request_id)
    assert request is not None
    assert request.status == ComplexTaskRequestStatus.FAILED
    assert request.final_outcome is not None
    segment2_id = request.task_segment_ids[1]
    segment2 = segment_store.get(segment2_id)
    assert segment2 is not None
    failed_graph_id = segment2.harness_graph_ids[0]
    failed_graph = graph_store.get(failed_graph_id)
    assert failed_graph is not None
    assert failed_graph.status == HarnessGraphStatus.FAILED
    assert failed_graph.fail_reason == HarnessGraphFailReason.STARTUP_FAILED
    assert request.final_outcome["final_segment_id"] == segment2_id
    assert request.final_outcome["final_harness_graph_id"] == failed_graph_id

    parent_final = task_store.get_task(parent_task_id)
    assert parent_final is not None
    assert parent_final["status"] == HarnessTaskStatus.FAILED.value


def test_delegated_retry_waits_until_final_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    runtime = _build_runtime(
        request_store, segment_store, graph_store, task_store
    )
    parent_task_id, parent_graph_id = _seed_outer_running_generator(
        runtime=runtime,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = ComplexTaskHandoffCoordinator(runtime=runtime)
    handoff = coordinator.start(
        task_center_run_id=task_center_run_id,
        parent_task_id=parent_task_id,
        parent_harness_graph_id=parent_graph_id,
        goal="delegated retry",
    )

    # Graph 1 fails — manager should retry inside same segment, parent waits.
    _drive_delegated_graph_to_fail(
        runtime=runtime, delegated_graph_id=handoff.initial_harness_graph_id
    )
    segment1 = segment_store.get(handoff.initial_segment_id)
    assert segment1 is not None
    assert len(segment1.harness_graph_ids) == 2
    parent_mid = task_store.get_task(parent_task_id)
    assert parent_mid is not None
    assert parent_mid["status"] == HarnessTaskStatus.WAITING_COMPLEX_TASK.value
    delegated_mid = request_store.get(handoff.complex_task_request_id)
    assert delegated_mid is not None
    assert delegated_mid.status == ComplexTaskRequestStatus.OPEN

    # Graph 2 passes terminally inside the same segment — final close.
    retry_graph_id = segment1.harness_graph_ids[1]
    _drive_delegated_graph_to_pass(
        runtime=runtime,
        delegated_graph_id=retry_graph_id,
        continuation_goal=None,
    )

    parent_final = task_store.get_task(parent_task_id)
    delegated_final = request_store.get(handoff.complex_task_request_id)
    refreshed_segment = segment_store.get(handoff.initial_segment_id)
    assert parent_final is not None
    assert parent_final["status"] == HarnessTaskStatus.DONE.value
    assert delegated_final is not None
    assert delegated_final.status == ComplexTaskRequestStatus.SUCCEEDED
    assert refreshed_segment is not None
    assert refreshed_segment.status == TaskSegmentStatus.SUCCEEDED

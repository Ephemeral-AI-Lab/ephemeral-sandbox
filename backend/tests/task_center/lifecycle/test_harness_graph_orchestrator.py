"""HarnessGraphOrchestrator lifecycle tests."""

from __future__ import annotations

import pytest

from task_center.mission.mission import ComplexTaskCloseReport
from task_center.exceptions import GraphInvariantViolation
from task_center.attempt import (
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import (
    AgentLaunch,
    HarnessGraphRuntime,
)
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    HarnessTaskRole,
    HarnessTaskStatus,
    PlannedGeneratorTask,
    PlannerFailureSubmission,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)
from task_center.episode.episode import TaskSegmentCreationReason


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailingRoleLauncher(_FakeLauncher):
    def __init__(self, role: HarnessTaskRole) -> None:
        super().__init__()
        self._role = role

    def launch(self, launch: AgentLaunch) -> None:
        if launch.role == self._role:
            raise RuntimeError(f"{self._role.value} launch failed")
        super().launch(launch)


def _seed_graph(request_store, segment_store, graph_store, task_center_run_id):
    request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="outer-task",
        goal="solve the task",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="solve the task",
        attempt_budget=2,
    )
    return graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)


def _build_orchestrator(
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
    *,
    composer,
    launcher=None,
):
    graph = _seed_graph(
        request_store, segment_store, graph_store, task_center_run_id
    )
    launcher = launcher or _FakeLauncher()
    registry = HarnessGraphOrchestratorRegistry()
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        composer=composer,
    )
    closed: list[str] = []
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=graph,
        on_graph_closed=closed.append,
        runtime=runtime,
    )
    registry.register(orchestrator)
    return orchestrator, graph, launcher, registry, closed


def _plan(
    graph_id: str,
    *,
    tasks: tuple[PlannedGeneratorTask, ...],
    kind: str = "full",
    continuation_goal: str | None = None,
) -> PlannerSubmission:
    return PlannerSubmission(
        graph_id=graph_id,
        planner_task_id=planner_task_id(graph_id),
        kind=kind,  # type: ignore[arg-type]
        task_specification="spec",
        evaluation_criteria=("criterion",),
        tasks=tasks,
        continuation_goal=continuation_goal,
        summary="plan accepted",
    )


def _generator_success(graph_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        graph_id=graph_id,
        task_id=generator_task_id(graph_id, local_id),
        outcome="success",
        summary=f"{local_id} done",
        payload={"role": "executor"},
    )


def _generator_failure(graph_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        graph_id=graph_id,
        task_id=generator_task_id(graph_id, local_id),
        outcome="failure",
        summary=f"{local_id} failed",
        payload={"role": "executor"},
    )


def _evaluator_submission(graph_id: str, outcome: str) -> EvaluatorSubmission:
    return EvaluatorSubmission(
        graph_id=graph_id,
        task_id=evaluator_task_id(graph_id),
        outcome=outcome,  # type: ignore[arg-type]
        summary=f"evaluation {outcome}",
        payload={},
    )


def test_start_creates_planner_task_and_sets_graph_planner_id(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, launcher, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )

    orchestrator.start()

    refreshed = graph_store.get(graph.id)
    task_id = planner_task_id(graph.id)
    planner_task = task_store.get_task(task_id)
    assert refreshed is not None and refreshed.planner_task_id == task_id
    assert planner_task is not None
    assert planner_task["status"] == HarnessTaskStatus.RUNNING.value
    assert [launch.task_id for launch in launcher.launches] == [task_id]


def test_apply_plan_submission_persists_contract_and_generator_ids(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, launcher, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    tasks = (
        PlannedGeneratorTask("a", "executor", (), "do A"),
        PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
    )

    orchestrator.apply_plan_submission(_plan(graph.id, tasks=tasks))

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.stage == HarnessGraphStage.GENERATING
    assert refreshed.task_specification == "spec"
    assert refreshed.generator_task_ids == (
        generator_task_id(graph.id, "a"),
        generator_task_id(graph.id, "b"),
    )
    assert [launch.task_id for launch in launcher.launches] == [
        planner_task_id(graph.id),
        generator_task_id(graph.id, "a"),
    ]


def test_apply_partial_plan_submission_stores_continuation_goal(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            kind="partial",
            continuation_goal="continue here",
        )
    )

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.continuation_goal == "continue here"


def test_apply_planner_failure_marks_task_and_closes_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, registry, closed = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    orchestrator.apply_planner_failure(
        PlannerFailureSubmission(
            graph_id=graph.id,
            planner_task_id=planner_task_id(graph.id),
            fail_reason="run_exhausted",
            summary="planner stopped",
        )
    )

    refreshed = graph_store.get(graph.id)
    task = task_store.get_task(planner_task_id(graph.id))
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.FAILED
    assert refreshed.fail_reason == HarnessGraphFailReason.PLANNER_FAILED
    assert task is not None and task["status"] == HarnessTaskStatus.FAILED.value
    assert closed == [graph.id]
    assert registry.get(graph.id) is None


def test_apply_generator_success_launches_newly_ready_dependents(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, launcher, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(graph.id, "a"))

    assert [launch.task_id for launch in launcher.launches] == [
        planner_task_id(graph.id),
        generator_task_id(graph.id, "a"),
        generator_task_id(graph.id, "b"),
    ]
    task_b = task_store.get_task(generator_task_id(graph.id, "b"))
    assert task_b is not None and task_b["status"] == "running"


def test_missing_generator_agent_profile_is_invariant_violation(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
            ),
        )
    )
    task_b_id = generator_task_id(graph.id, "b")
    task_b = task_store.get_task(task_b_id)
    assert task_b is not None
    task_store.upsert_task(
        task_id=task_b_id,
        task_center_run_id=task_b["task_center_run_id"],
        role=task_b["role"],
        agent_name=None,
        task_input=task_b["task_input"],
        status=task_b["status"],
        summaries=task_b["summaries"],
        needs=task_b["needs"],
        task_center_harness_graph_id=task_b["task_center_harness_graph_id"],
        spawn_reason=task_b["spawn_reason"],
    )

    with pytest.raises(GraphInvariantViolation):
        orchestrator.apply_generator_submission(_generator_success(graph.id, "a"))

    refreshed_task_b = task_store.get_task(task_b_id)
    assert refreshed_task_b is not None
    assert refreshed_task_b["status"] == HarnessTaskStatus.PENDING.value


def test_generator_launch_failure_marks_task_failed_and_closes_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(HarnessTaskRole.GENERATOR)
    orchestrator, graph, _, registry, closed = _build_orchestrator(
        request_store,
        segment_store,
        graph_store,
        task_store,
        task_center_run_id,
        launcher=launcher,
        composer=composer,
    )
    orchestrator.start()

    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    task = task_store.get_task(generator_task_id(graph.id, "a"))
    refreshed = graph_store.get(graph.id)
    assert task is not None
    assert task["status"] == HarnessTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.FAILED
    assert refreshed.fail_reason == HarnessGraphFailReason.GENERATOR_FAILED
    assert closed == [graph.id]
    assert registry.get(graph.id) is None


def test_evaluator_launch_failure_marks_task_failed_and_closes_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(HarnessTaskRole.EVALUATOR)
    orchestrator, graph, _, registry, closed = _build_orchestrator(
        request_store,
        segment_store,
        graph_store,
        task_store,
        task_center_run_id,
        launcher=launcher,
        composer=composer,
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(graph.id, "a"))

    task = task_store.get_task(evaluator_task_id(graph.id))
    refreshed = graph_store.get(graph.id)
    assert task is not None
    assert task["status"] == HarnessTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.FAILED
    assert refreshed.fail_reason == HarnessGraphFailReason.EVALUATOR_FAILED
    assert closed == [graph.id]
    assert registry.get(graph.id) is None


def test_waiting_complex_task_prevents_generator_quiescence(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, closed = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
        )
    )
    task_store.set_task_status(
        generator_task_id(graph.id, "a"),
        status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
    )

    orchestrator.apply_generator_submission(_generator_success(graph.id, "b"))

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.stage == HarnessGraphStage.GENERATING
    assert closed == []


def test_complex_task_close_report_success_resumes_waiting_generator(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    task_id = generator_task_id(graph.id, "a")
    task_store.set_task_status(
        task_id,
        status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
    )

    orchestrator.apply_complex_task_close_report(
        ComplexTaskCloseReport(
            complex_task_request_id="delegated-1",
            requested_by_task_id=task_id,
            outcome="success",
            final_segment_id="segment-1",
            final_harness_graph_id="graph-1",
        )
    )

    task = task_store.get_task(task_id)
    refreshed = graph_store.get(graph.id)
    assert task is not None
    assert task["status"] == HarnessTaskStatus.DONE.value
    assert task["summaries"][-1]["payload"]["complex_task_close_report"][
        "complex_task_request_id"
    ] == "delegated-1"
    assert refreshed is not None
    assert refreshed.stage == HarnessGraphStage.EVALUATING


def test_complex_task_close_report_failure_blocks_dependents_and_closes_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, closed = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
        )
    )
    task_id = generator_task_id(graph.id, "a")
    dependent_id = generator_task_id(graph.id, "b")
    task_store.set_task_status(
        task_id,
        status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
    )

    orchestrator.apply_complex_task_close_report(
        ComplexTaskCloseReport(
            complex_task_request_id="delegated-1",
            requested_by_task_id=task_id,
            outcome="failed",
            final_segment_id="segment-1",
            final_harness_graph_id="graph-1",
        )
    )

    task = task_store.get_task(task_id)
    dependent = task_store.get_task(dependent_id)
    refreshed = graph_store.get(graph.id)
    assert task is not None
    assert task["status"] == HarnessTaskStatus.FAILED.value
    assert dependent is not None
    assert dependent["status"] == HarnessTaskStatus.BLOCKED.value
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.FAILED
    assert closed == [graph.id]


def test_apply_generator_failure_blocks_pending_descendants(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
                PlannedGeneratorTask("c", "executor", ("b",), "do C"),
                PlannedGeneratorTask("d", "executor", (), "do D"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_failure(graph.id, "a"))

    task_b = task_store.get_task(generator_task_id(graph.id, "b"))
    task_c = task_store.get_task(generator_task_id(graph.id, "c"))
    task_d = task_store.get_task(generator_task_id(graph.id, "d"))
    assert task_b is not None and task_b["status"] == "blocked"
    assert task_c is not None and task_c["status"] == "blocked"
    assert task_d is not None and task_d["status"] == "running"


def test_generator_failure_waits_then_closes_after_quiescence(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, closed = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_failure(graph.id, "a"))
    assert closed == []

    orchestrator.apply_generator_submission(_generator_success(graph.id, "b"))

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.FAILED
    assert refreshed.fail_reason == HarnessGraphFailReason.GENERATOR_FAILED
    assert closed == [graph.id]


def test_all_generators_done_spawns_evaluator(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, launcher, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(graph.id, "a"))

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.stage == HarnessGraphStage.EVALUATING
    assert launcher.launches[-1].task_id == evaluator_task_id(graph.id)


def test_apply_evaluator_success_closes_graph_passed(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, closed = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(graph.id, "a"))

    orchestrator.apply_evaluator_submission(
        _evaluator_submission(graph.id, "success")
    )

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.PASSED
    assert refreshed.fail_reason is None
    assert closed == [graph.id]


def test_apply_evaluator_failure_closes_graph_failed(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, closed = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(graph.id, "a"))

    orchestrator.apply_evaluator_submission(
        _evaluator_submission(graph.id, "failure")
    )

    refreshed = graph_store.get(graph.id)
    assert refreshed is not None
    assert refreshed.status == HarnessGraphStatus.FAILED
    assert refreshed.fail_reason == HarnessGraphFailReason.EVALUATOR_FAILED
    assert closed == [graph.id]


def test_orchestrator_rejects_submit_request_plan_shape(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    with pytest.raises(AttributeError):
        getattr(orchestrator, "apply_request_plan")


def test_orchestrator_never_creates_retry_graph(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
):
    orchestrator, graph, _, _, _ = _build_orchestrator(
        request_store, segment_store, graph_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            graph.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_failure(graph.id, "a"))

    assert len(graph_store.list_for_segment(graph.task_segment_id)) == 1

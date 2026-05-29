"""AttemptOrchestrator lifecycle tests."""

from __future__ import annotations

import pytest

from task_center.workflow.state import WorkflowClosureReport, WorkflowOriginKind
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.attempt import (
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.deps import (
    AgentLaunch,
    AttemptDeps,
)
from task_center._core.task_state import TaskCenterTaskRole, TaskCenterTaskStatus
from task_center.submissions import EvaluatorSubmission, GeneratorSubmission, PlannedGeneratorTask, PlannerFailureSubmission, PlannerSubmission
from task_center._core.primitives import evaluator_task_id, generator_task_id, planner_task_id
from task_center.iteration.state import IterationCreationReason


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailingRoleLauncher(_FakeLauncher):
    def __init__(self, role: TaskCenterTaskRole) -> None:
        super().__init__()
        self._role = role

    def launch(self, launch: AgentLaunch) -> None:
        if launch.role == self._role:
            raise RuntimeError(f"{self._role.value} launch failed")
        super().launch(launch)


class _FailingEvaluatorComposer:
    def __init__(self, inner) -> None:
        self._inner = inner
        self.engine = inner.engine

    def compose(self, *, base_agent_name: str, scope):
        if base_agent_name == "evaluator":
            raise RuntimeError("evaluator compose failed")
        return self._inner.compose(base_agent_name=base_agent_name, scope=scope)


def _seed_graph(workflow_store, iteration_store, attempt_store, task_center_run_id):
    request = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="outer-task",
        goal="solve the task",
    )
    iteration = iteration_store.insert(
        workflow_id=request.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="solve the task",
        attempt_budget=2,
    )
    return attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)


def _build_orchestrator(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    *,
    composer,
    launcher=None,
):
    attempt = _seed_graph(
        workflow_store, iteration_store, attempt_store, task_center_run_id
    )
    launcher = launcher or _FakeLauncher()
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        composer=composer,
    )
    closed: list[str] = []
    orchestrator = AttemptOrchestrator(
        attempt=attempt,
        on_attempt_closed=closed.append,
        runtime=runtime,
    )
    registry.register(orchestrator)
    return orchestrator, attempt, launcher, registry, closed


def _plan(
    attempt_id: str,
    *,
    tasks: tuple[PlannedGeneratorTask, ...],
    kind: str = "full",
    deferred_goal_for_next_iteration: str | None = None,
) -> PlannerSubmission:
    return PlannerSubmission(
        attempt_id=attempt_id,
        planner_task_id=planner_task_id(attempt_id),
        kind=kind,  # type: ignore[arg-type]
        plan_spec="spec",
        evaluation_criteria=("criterion",),
        tasks=tasks,
        deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
        summary="plan accepted",
    )


def _generator_success(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        outcome="success",
        summary=f"{local_id} done",
        payload={"role": "executor"},
    )


def _generator_failure(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        outcome="failure",
        summary=f"{local_id} failed",
        payload={"role": "executor"},
    )


def _generator_blocker(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        outcome="blocker",
        summary=f"{local_id} blocked",
        payload={"role": "executor"},
    )


def _evaluator_submission(attempt_id: str, outcome: str) -> EvaluatorSubmission:
    return EvaluatorSubmission(
        attempt_id=attempt_id,
        task_id=evaluator_task_id(attempt_id),
        outcome=outcome,  # type: ignore[arg-type]
        summary=f"evaluation {outcome}",
        payload={},
    )


def test_start_creates_planner_task_and_sets_graph_planner_id(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )

    orchestrator.start()

    refreshed = attempt_store.get(attempt.id)
    task_id = planner_task_id(attempt.id)
    planner_task = task_store.get_task(task_id)
    assert refreshed is not None and refreshed.planner_task_id == task_id
    assert planner_task is not None
    assert planner_task["status"] == TaskCenterTaskStatus.RUNNING.value
    assert [launch.task_id for launch in launcher.launches] == [task_id]


def test_apply_plan_submission_persists_contract_and_generator_ids(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    tasks = (
        PlannedGeneratorTask("a", "executor", (), "do A"),
        PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
    )

    orchestrator.apply_plan_submission(_plan(attempt.id, tasks=tasks))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.GENERATE
    assert refreshed.plan_spec == "spec"
    assert refreshed.generator_task_ids == (
        generator_task_id(attempt.id, "a"),
        generator_task_id(attempt.id, "b"),
    )
    assert [launch.task_id for launch in launcher.launches] == [
        planner_task_id(attempt.id),
        generator_task_id(attempt.id, "a"),
    ]


def test_apply_partial_plan_submission_stores_deferred_goal(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            kind="defers",
            deferred_goal_for_next_iteration="continue here",
        )
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.deferred_goal_for_next_iteration == "continue here"


def test_apply_planner_failure_marks_task_and_closes_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    orchestrator.apply_planner_failure(
        PlannerFailureSubmission(
            attempt_id=attempt.id,
            planner_task_id=planner_task_id(attempt.id),
            fail_reason="run_exhausted",
            summary="planner stopped",
        )
    )

    refreshed = attempt_store.get(attempt.id)
    task = task_store.get_task(planner_task_id(attempt.id))
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.PLANNER_FAILED
    assert task is not None and task["status"] == TaskCenterTaskStatus.FAILED.value
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_apply_generator_success_launches_newly_ready_dependents(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    assert [launch.task_id for launch in launcher.launches] == [
        planner_task_id(attempt.id),
        generator_task_id(attempt.id, "a"),
        generator_task_id(attempt.id, "b"),
    ]
    task_b = task_store.get_task(generator_task_id(attempt.id, "b"))
    assert task_b is not None and task_b["status"] == "running"


def test_missing_generator_agent_profile_is_invariant_violation(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
            ),
        )
    )
    task_b_id = generator_task_id(attempt.id, "b")
    task_b = task_store.get_task(task_b_id)
    assert task_b is not None
    task_store.upsert_task(
        task_id=task_b_id,
        task_center_run_id=task_b["task_center_run_id"],
        role=task_b["role"],
        agent_name=None,
        context_message=task_b["context_message"],
        status=task_b["status"],
        summaries=task_b["summaries"],
        needs=task_b["needs"],
        task_center_attempt_id=task_b["task_center_attempt_id"],
        spawn_reason=task_b["spawn_reason"],
    )

    with pytest.raises(TaskCenterInvariantViolation):
        orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    refreshed_task_b = task_store.get_task(task_b_id)
    assert refreshed_task_b is not None
    assert refreshed_task_b["status"] == TaskCenterTaskStatus.PENDING.value


def test_generator_launch_failure_marks_task_failed_and_closes_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(TaskCenterTaskRole.GENERATOR)
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        task_center_run_id,
        launcher=launcher,
        composer=composer,
    )
    orchestrator.start()

    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    task = task_store.get_task(generator_task_id(attempt.id, "a"))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.GENERATOR_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_evaluator_launch_failure_marks_task_failed_and_closes_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(TaskCenterTaskRole.EVALUATOR)
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        task_center_run_id,
        launcher=launcher,
        composer=composer,
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    task = task_store.get_task(evaluator_task_id(attempt.id))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_evaluator_compose_failure_closes_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        task_center_run_id,
        composer=_FailingEvaluatorComposer(composer),
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    with pytest.raises(RuntimeError, match="evaluator compose failed"):
        orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    task = task_store.get_task(evaluator_task_id(attempt.id))
    refreshed = attempt_store.get(attempt.id)
    assert task is None
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_waiting_workflow_prevents_generator_quiescence(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
        )
    )
    task_store.set_task_status(
        generator_task_id(attempt.id, "a"),
        status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "b"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.GENERATE
    assert closed == []


def test_workflow_closure_report_success_resumes_waiting_generator(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    task_store.set_task_status(
        task_id,
        status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
    )

    orchestrator.apply_workflow_closure_report(
        WorkflowClosureReport(
            workflow_id="delegated-1",
            task_center_run_id=task_center_run_id,
            origin_kind=WorkflowOriginKind.TASK,
            requested_by_task_id=task_id,
            outcome="success",
            final_iteration_id="iteration-1",
            final_attempt_id="attempt-1",
        )
    )

    task = task_store.get_task(task_id)
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert task["summaries"][-1]["payload"]["workflow_closure_report"][
        "workflow_id"
    ] == "delegated-1"
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.EVALUATE


def test_workflow_closure_report_failure_leaves_dependents_pending_and_closes_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    dependent_id = generator_task_id(attempt.id, "b")
    task_store.set_task_status(
        task_id,
        status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
    )

    orchestrator.apply_workflow_closure_report(
        WorkflowClosureReport(
            workflow_id="delegated-1",
            task_center_run_id=task_center_run_id,
            origin_kind=WorkflowOriginKind.TASK,
            requested_by_task_id=task_id,
            outcome="failed",
            final_iteration_id="iteration-1",
            final_attempt_id="attempt-1",
        )
    )

    task = task_store.get_task(task_id)
    dependent = task_store.get_task(dependent_id)
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert dependent is not None
    assert dependent["status"] == TaskCenterTaskStatus.PENDING.value
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert closed == [attempt.id]


def test_apply_generator_blocker_leaves_pending_descendants_not_started(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
                PlannedGeneratorTask("c", "executor", ("b",), "do C"),
                PlannedGeneratorTask("d", "executor", (), "do D"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_blocker(attempt.id, "a"))

    task_a = task_store.get_task(generator_task_id(attempt.id, "a"))
    task_b = task_store.get_task(generator_task_id(attempt.id, "b"))
    task_c = task_store.get_task(generator_task_id(attempt.id, "c"))
    task_d = task_store.get_task(generator_task_id(attempt.id, "d"))
    assert task_a is not None and task_a["status"] == "blocked"
    assert task_b is not None and task_b["status"] == "pending"
    assert task_c is not None and task_c["status"] == "pending"
    assert task_d is not None and task_d["status"] == "running"


def test_generator_blocker_waits_then_closes_after_runnable_siblings_finish(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_blocker(attempt.id, "a"))
    assert closed == []

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "b"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.GENERATOR_FAILED
    assert closed == [attempt.id]


def test_all_generators_done_spawns_evaluator(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.EVALUATE
    assert launcher.launches[-1].task_id == evaluator_task_id(attempt.id)


def test_apply_evaluator_success_closes_graph_passed(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    orchestrator.apply_evaluator_submission(
        _evaluator_submission(attempt.id, "success")
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.PASSED
    assert refreshed.fail_reason is None
    assert closed == [attempt.id]


def test_apply_evaluator_failure_closes_graph_failed(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    orchestrator.apply_evaluator_submission(
        _evaluator_submission(attempt.id, "failure")
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed == [attempt.id]


def test_orchestrator_never_creates_retry_attempt(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_failure(attempt.id, "a"))

    assert len(attempt_store.list_for_iteration(attempt.iteration_id)) == 1

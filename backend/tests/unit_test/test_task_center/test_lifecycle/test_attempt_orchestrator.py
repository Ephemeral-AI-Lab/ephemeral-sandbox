"""AttemptOrchestrator lifecycle tests.

The orchestrator runs one planner -> plan-DAG attempt. The plan is a DAG of
GENERATOR + REDUCER tasks scheduled as a single RUN stage (PLAN -> RUN ->
CLOSED). An attempt PASSES only when every plan task (generators and reducers)
reaches DONE; any failed/blocked plan task closes it FAILED with TASK_FAILED.
There is no evaluator stage and no closure-report DTO: a delegated child
workflow resolves through ``start/apply/cancel_child_workflow``.
"""

from __future__ import annotations

import pytest

from task_center._core.primitives import TaskCenterInvariantViolation
from task_center._core.state import (
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
    Workflow,
    WorkflowStatus,
)
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.launch import (
    AgentLaunch,
    AttemptDeps,
)
from task_center._core.task_state import TaskCenterTaskRole, TaskCenterTaskStatus
from task_center.submissions import (
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannedReducerTask,
    PlannerFailureSubmission,
    PlannerSubmission,
    ReducerSubmission,
)
from task_center._core.primitives import (
    generator_task_id,
    planner_task_id,
    reducer_task_id,
)
from task_center._core.state import IterationCreationReason


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


class _FailingReducerComposer:
    """Wraps a real composer but raises when composing the reducer launch."""

    def __init__(self, inner) -> None:
        self._inner = inner
        self.engine = inner.engine

    def compose(self, *, base_agent_name: str, scope):
        if base_agent_name == "reducer":
            raise RuntimeError("reducer compose failed")
        return self._inner.compose(base_agent_name=base_agent_name, scope=scope)


def _seed_graph(workflow_store, iteration_store, attempt_store, task_center_run_id):
    workflow = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        parent_task_id="outer-task",
        workflow_goal="solve the task",
    )
    iteration = iteration_store.insert(
        workflow_id=workflow.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        iteration_goal="solve the task",
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
    generators: tuple[PlannedGeneratorTask, ...],
    reducers: tuple[PlannedReducerTask, ...],
    kind: str = "completes",
    deferred_goal_for_next_iteration: str | None = None,
) -> PlannerSubmission:
    return PlannerSubmission(
        attempt_id=attempt_id,
        planner_task_id=planner_task_id(attempt_id),
        kind=kind,  # type: ignore[arg-type]
        generators=generators,
        reducers=reducers,
        deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
    )


def _one_reducer(needs: tuple[str, ...]) -> tuple[PlannedReducerTask, ...]:
    return (PlannedReducerTask("r", needs, "judge it"),)


def _generator_success(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        status="success",
        outcome=f"{local_id} done",
        terminal_tool_result={"role": "executor"},
    )


def _generator_failure(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        status="failed",
        outcome=f"{local_id} failed",
        terminal_tool_result={"role": "executor"},
    )


def _generator_blocker(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        status="failed",
        outcome=f"{local_id} blocked",
        terminal_tool_result={"role": "executor"},
    )


def _reducer_submission(attempt_id: str, status: str) -> ReducerSubmission:
    return ReducerSubmission(
        attempt_id=attempt_id,
        task_id=reducer_task_id(attempt_id, "r"),
        status=status,  # type: ignore[arg-type]
        outcome=f"reduction {status}",
        terminal_tool_result={},
    )


def test_start_creates_planner_task_and_sets_attempt_planner_id(
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


def test_apply_plan_submission_runs_and_persists_plan_task_ids(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    tasks = (
        PlannedGeneratorTask("a", "executor", (), "do A"),
        PlannedGeneratorTask("b", "generator", ("a",), "do B"),
    )

    orchestrator.apply_plan_submission(
        _plan(attempt.id, generators=tasks, reducers=_one_reducer(("a", "b")))
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.RUN
    assert refreshed.generator_task_ids == (
        generator_task_id(attempt.id, "a"),
        generator_task_id(attempt.id, "b"),
    )
    assert refreshed.reducer_task_ids == (reducer_task_id(attempt.id, "r"),)
    # Only the root generator ``a`` is ready immediately.
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
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
            kind="defers",
            deferred_goal_for_next_iteration="continue here",
        )
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.deferred_goal_for_next_iteration == "continue here"


def test_apply_planner_failure_marks_task_and_closes_attempt(
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
        )
    )

    refreshed = attempt_store.get(attempt.id)
    task = task_store.get_task(planner_task_id(attempt.id))
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.TASK_FAILED
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
            generators=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "generator", ("a",), "do B"),
            ),
            reducers=_one_reducer(("a", "b")),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    # The reducer needs both ``a`` and ``b``, so only ``b`` becomes ready here;
    # the launch sequence is deterministic.
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
            generators=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "generator", ("a",), "do B"),
            ),
            reducers=_one_reducer(("a", "b")),
        )
    )
    task_b_id = generator_task_id(attempt.id, "b")
    task_b = task_store.get_task(task_b_id)
    assert task_b is not None
    # Strip the persisted agent profile so the launch cannot resolve a target.
    task_store.upsert_task(
        task_id=task_b_id,
        task_center_run_id=task_b["task_center_run_id"],
        role=task_b["role"],
        agent_name=None,
        context_message=task_b["context_message"],
        status=task_b["status"],
        outcomes=task_b["outcomes"],
        needs=task_b["needs"],
    )

    with pytest.raises(TaskCenterInvariantViolation):
        orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    refreshed_task_b = task_store.get_task(task_b_id)
    assert refreshed_task_b is not None
    assert refreshed_task_b["status"] == TaskCenterTaskStatus.PENDING.value


def test_generator_launch_failure_marks_task_failed_and_closes_attempt(
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
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )

    task = task_store.get_task(generator_task_id(attempt.id, "a"))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["terminal_tool_result"]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.TASK_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_reducer_launch_failure_marks_task_failed_and_closes_attempt(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(TaskCenterTaskRole.REDUCER)
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
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    task = task_store.get_task(reducer_task_id(attempt.id, "r"))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["terminal_tool_result"]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.TASK_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_reducer_compose_failure_marks_task_failed_and_closes_attempt(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        task_center_run_id,
        composer=_FailingReducerComposer(composer),
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )

    # All generators done -> reducer launch is attempted; compose raises, so the
    # reducer task is marked FAILED and the attempt closes FAILED (no re-raise).
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    task = task_store.get_task(reducer_task_id(attempt.id, "r"))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["terminal_tool_result"]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.TASK_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


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
            generators=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
                PlannedGeneratorTask("c", "executor", ("b",), "do C"),
                PlannedGeneratorTask("d", "executor", (), "do D"),
            ),
            reducers=_one_reducer(("c", "d")),
        )
    )

    orchestrator.apply_generator_submission(_generator_blocker(attempt.id, "a"))

    task_a = task_store.get_task(generator_task_id(attempt.id, "a"))
    task_b = task_store.get_task(generator_task_id(attempt.id, "b"))
    task_c = task_store.get_task(generator_task_id(attempt.id, "c"))
    task_d = task_store.get_task(generator_task_id(attempt.id, "d"))
    assert task_a is not None and task_a["status"] == "failed"
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
            generators=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
            reducers=_one_reducer(("a", "b")),
        )
    )

    orchestrator.apply_generator_submission(_generator_blocker(attempt.id, "a"))
    assert closed == []

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "b"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.TASK_FAILED
    assert closed == [attempt.id]


def test_all_generators_done_launches_reducer(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.RUN
    assert launcher.launches[-1].task_id == reducer_task_id(attempt.id, "r")
    reducer_task = task_store.get_task(reducer_task_id(attempt.id, "r"))
    assert reducer_task is not None and reducer_task["status"] == "running"


def test_reducer_success_closes_attempt_passed(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    orchestrator.apply_reducer_submission(_reducer_submission(attempt.id, "success"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.PASSED
    assert refreshed.fail_reason is None
    assert closed == [attempt.id]


def test_reducer_failure_closes_attempt_failed(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    orchestrator.apply_reducer_submission(_reducer_submission(attempt.id, "failure"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.TASK_FAILED
    assert closed == [attempt.id]


# ---- child-workflow handoff -------------------------------------------------


def _waiting_workflow(task_center_run_id: str, *, status: WorkflowStatus, parent_task_id: str) -> Workflow:
    from datetime import UTC, datetime

    now = datetime.now(UTC)
    return Workflow(
        id="delegated-1",
        task_center_run_id=task_center_run_id,
        workflow_goal="child",
        status=status,
        iteration_ids=(),
        parent_task_id=parent_task_id,
        created_at=now,
        updated_at=now,
        closed_at=now,
    )


def test_child_workflow_success_resumes_waiting_generator(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    child = _waiting_workflow(
        task_center_run_id, status=WorkflowStatus.SUCCEEDED, parent_task_id=task_id
    )
    orchestrator.start_child_workflow(
        generator_task=task_store.get_task(task_id), child_workflow=child
    )
    assert task_store.get_task(task_id)["status"] == TaskCenterTaskStatus.WAITING_WORKFLOW.value

    orchestrator.apply_child_workflow_outcome(
        generator_task=task_store.get_task(task_id),
        child_workflow=child,
        final_attempt_id=None,
    )

    task = task_store.get_task(task_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert task["terminal_tool_result"]["child_workflow_id"] == "delegated-1"
    # Generator done -> reducer became ready and launched.
    reducer_task = task_store.get_task(reducer_task_id(attempt.id, "r"))
    assert reducer_task is not None and reducer_task["status"] == "running"


def test_child_workflow_failure_leaves_dependents_pending_and_closes_attempt(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
            reducers=_one_reducer(("a", "b")),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    dependent_id = generator_task_id(attempt.id, "b")
    child = _waiting_workflow(
        task_center_run_id, status=WorkflowStatus.FAILED, parent_task_id=task_id
    )
    orchestrator.start_child_workflow(
        generator_task=task_store.get_task(task_id), child_workflow=child
    )

    orchestrator.apply_child_workflow_outcome(
        generator_task=task_store.get_task(task_id),
        child_workflow=child,
        final_attempt_id=None,
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


def test_cancel_child_workflow_restores_generator_running(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    child = _waiting_workflow(
        task_center_run_id, status=WorkflowStatus.OPEN, parent_task_id=task_id
    )
    orchestrator.start_child_workflow(
        generator_task=task_store.get_task(task_id), child_workflow=child
    )

    orchestrator.cancel_child_workflow(generator_task=task_store.get_task(task_id))

    task = task_store.get_task(task_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.RUNNING.value


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
            generators=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            reducers=_one_reducer(("a",)),
        )
    )
    orchestrator.apply_generator_submission(_generator_failure(attempt.id, "a"))

    assert len(attempt_store.list_for_iteration(attempt.iteration_id)) == 1

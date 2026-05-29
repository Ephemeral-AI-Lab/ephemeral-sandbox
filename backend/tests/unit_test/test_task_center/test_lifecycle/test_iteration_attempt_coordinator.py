"""IterationAttemptCoordinator lifecycle tests."""

from __future__ import annotations

import json

import pytest

from task_center.iteration import IterationAttemptCoordinator
from task_center.attempt import (
    AttemptFailReason,
    AttemptStatus,
)
from task_center.iteration.state import (
    AttemptPlanFailed,
    SuccessDeferred,
    IterationClosureReport,
    TerminalSuccess,
)
from task_center.iteration.state import (
    IterationCreationReason,
    IterationStatus,
)


def _seed_segment(
    workflow_store, iteration_store, task_center_run_id, attempt_budget=2
) -> str:
    req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = iteration_store.insert(
        workflow_id=req.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=attempt_budget,
    )
    return seg.id


def _make_coordinator(seg_id, iteration_store, attempt_store):
    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
    )
    return coordinator, captured


class _StartedOrchestrator:
    def __init__(self, attempt_id: str, started: list[str]) -> None:
        self.attempt_id = attempt_id
        self._started = started

    def start(self) -> None:
        self._started.append(self.attempt_id)


class _FailingStartOrchestrator:
    def __init__(self, attempt_id: str) -> None:
        self.attempt_id = attempt_id

    def start(self) -> None:
        raise RuntimeError("orchestrator start failed")


def test_initial_iteration_creates_graph_sequence_1(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """Phase 01 exit: create iteration 1 with harness attempt sequence 1."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    coordinator, _ = _make_coordinator(seg_id, iteration_store, attempt_store)
    g = coordinator.create_initial_attempt()
    assert g.attempt_sequence_no == 1
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.attempt_ids == (g.id,)


def test_retry_creates_graph_in_same_segment(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """Phase 01 exit: retry creates another Attempt in the same iteration."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    coordinator, _ = _make_coordinator(seg_id, iteration_store, attempt_store)
    g1 = coordinator.create_initial_attempt()
    g2 = coordinator.create_next_attempt(previous_attempt_id=g1.id)
    assert g2.iteration_id == seg_id
    assert g2.attempt_sequence_no == 2
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.attempt_ids == (g1.id, g2.id)


def test_passing_graph_with_null_continuation_emits_terminal_success(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    coordinator, captured = _make_coordinator(seg_id, iteration_store, attempt_store)
    g = coordinator.create_initial_attempt()
    # No deferred_goal_for_next_iteration set on the attempt.
    attempt_store.close(
        g.id, status=AttemptStatus.PASSED, fail_reason=None
    )
    coordinator.handle_attempt_closed(g.id)
    assert len(captured) == 1
    assert isinstance(captured[0].outcome, TerminalSuccess)
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.status == IterationStatus.SUCCEEDED


class _FakeTaskStore:
    """Minimal TaskStoreProtocol surface: returns task rows by id.

    The coordinator builds the iteration's denormalized achieved record from
    the passing attempt's generator tasks via ``task_store.get_task`` per
    ``generator_task_id``.
    """

    def __init__(self, rows: dict[str, dict] | None = None) -> None:
        self._rows = rows or {}

    def get_task(self, task_id: str):
        return self._rows.get(task_id)


def _make_coordinator_with_task_store(seg_id, iteration_store, attempt_store, task_store):
    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        task_store=task_store,
    )
    return coordinator, captured


def test_close_iteration_passed_writes_structured_achieved_record(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """At successful close the coordinator denormalizes the passing attempt's
    GENERATOR tasks onto ``Iteration.task_summary`` as a JSON achieved record
    (``[{local_id, status, summary}, ...]``). Renamed from
    ``..._appends_passed_criteria``: the evaluator free-text + ``Passed
    criteria:`` block behavior was removed."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    g = attempt_store.insert(iteration_id=seg_id, attempt_sequence_no=1)
    iteration_store.append_attempt_id(seg_id, g.id)
    gen_a = f"{g.id}:gen:gen_a"
    gen_b = f"{g.id}:gen:gen_b"
    attempt_store.set_generator_task_ids(g.id, [gen_a, gen_b])
    attempt_store.close(g.id, status=AttemptStatus.PASSED, fail_reason=None)
    task_store = _FakeTaskStore(
        {
            gen_a: {"status": "done", "summaries": [{"summary": "Implemented storage layer."}]},
            gen_b: {"status": "done", "summaries": [{"summary": "Added the add command."}]},
        }
    )
    coordinator, _ = _make_coordinator_with_task_store(
        seg_id, iteration_store, attempt_store, task_store
    )
    coordinator.handle_attempt_closed(g.id)
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.task_summary is not None
    record = json.loads(seg.task_summary)
    assert record == [
        {"local_id": "gen_a", "status": "success", "summary": "Implemented storage layer."},
        {"local_id": "gen_b", "status": "success", "summary": "Added the add command."},
    ]


def test_close_iteration_passed_achieved_record_empty_without_generators(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """A passing attempt with no generators yields an empty JSON achieved
    record. Renamed from ``..._omits_criteria_when_payload_empty``: the
    evaluator-payload ``passed_criteria`` behavior was removed."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    g = attempt_store.insert(iteration_id=seg_id, attempt_sequence_no=1)
    iteration_store.append_attempt_id(seg_id, g.id)
    attempt_store.close(g.id, status=AttemptStatus.PASSED, fail_reason=None)
    coordinator, _ = _make_coordinator_with_task_store(
        seg_id, iteration_store, attempt_store, _FakeTaskStore()
    )
    coordinator.handle_attempt_closed(g.id)
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert json.loads(seg.task_summary) == []


def test_passing_graph_with_continuation_emits_success_continue(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    coordinator, captured = _make_coordinator(seg_id, iteration_store, attempt_store)
    g = coordinator.create_initial_attempt()
    attempt_store.set_plan_contract(
        g.id,
        plan_spec="spec",
        evaluation_criteria=["c1"],
        deferred_goal_for_next_iteration="next-goal",
    )
    attempt_store.close(
        g.id, status=AttemptStatus.PASSED, fail_reason=None
    )
    coordinator.handle_attempt_closed(g.id)
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, SuccessDeferred)
    assert outcome.deferred_goal_for_next_iteration == "next-goal"
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.deferred_goal_for_next_iteration == "next-goal"


def test_passing_graph_does_not_retry(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """Spec rule: passing attempt always closes the iteration; no second attempt."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    coordinator, _ = _make_coordinator(seg_id, iteration_store, attempt_store)
    g = coordinator.create_initial_attempt()
    attempt_store.close(
        g.id, status=AttemptStatus.PASSED, fail_reason=None
    )
    coordinator.handle_attempt_closed(g.id)
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.attempt_ids == (g.id,)
    assert seg.status == IterationStatus.SUCCEEDED


def test_failed_attempt_with_budget_creates_next_graph(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=2)
    coordinator, captured = _make_coordinator(seg_id, iteration_store, attempt_store)
    g1 = coordinator.create_initial_attempt()
    attempt_store.close(
        g1.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g1.id)
    assert captured == []  # No closure report yet — iteration still open.
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.is_open
    assert len(seg.attempt_ids) == 2


def test_failed_partial_plan_graph_retries_without_propagating_continuation(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=2)
    coordinator, captured = _make_coordinator(seg_id, iteration_store, attempt_store)
    g1 = coordinator.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id,
        plan_spec="partial slice",
        evaluation_criteria=["slice passes"],
        deferred_goal_for_next_iteration="next slice",
    )
    attempt_store.close(
        g1.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    coordinator.handle_attempt_closed(g1.id)

    assert captured == []
    seg = iteration_store.get(seg_id)
    assert seg is not None
    assert seg.is_open
    assert seg.deferred_goal_for_next_iteration is None
    assert len(seg.attempt_ids) == 2


def test_coordinator_starts_orchestrator_when_factory_present(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id, started)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )

    attempt = coordinator.create_initial_attempt()

    assert started == [attempt.id]


def test_initial_graph_start_can_be_deferred(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id, started)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )

    attempt = coordinator.create_unstarted_initial_attempt()
    assert started == []

    coordinator.start_attempt(attempt)

    assert started == [attempt.id]


def test_initial_start_failure_closes_inserted_graph(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _FailingStartOrchestrator(attempt.id)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )

    with pytest.raises(RuntimeError, match="orchestrator start failed"):
        coordinator.create_initial_attempt()

    iteration = iteration_store.get(seg_id)
    assert iteration is not None
    assert len(iteration.attempt_ids) == 1
    attempt = attempt_store.get(iteration.attempt_ids[0])
    assert attempt is not None
    assert attempt.status == AttemptStatus.FAILED
    assert attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert captured == []


def test_deferred_start_failure_closes_inserted_graph(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _FailingStartOrchestrator(attempt.id)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )

    attempt = coordinator.create_unstarted_initial_attempt()

    with pytest.raises(RuntimeError, match="orchestrator start failed"):
        coordinator.start_attempt(attempt)

    latest = attempt_store.get(attempt.id)
    assert latest is not None
    assert latest.status == AttemptStatus.FAILED
    assert latest.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert captured == []


def test_retry_start_failure_exhausts_budget_and_emits_closure(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """Retry-path startup failure closes the new attempt STARTUP_FAILED and,
    when budget is exhausted, emits ``attempt_plan_failed`` instead of
    leaving the iteration open."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=2)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        if attempt.attempt_sequence_no == 1:
            return _StartedOrchestrator(attempt.id, started)
        return _FailingStartOrchestrator(attempt.id)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )
    first_graph = coordinator.create_initial_attempt()
    attempt_store.close(
        first_graph.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    coordinator.handle_attempt_closed(first_graph.id)

    iteration = iteration_store.get(seg_id)
    assert iteration is not None
    assert len(iteration.attempt_ids) == 2
    retry_attempt = attempt_store.get(iteration.attempt_ids[-1])
    assert retry_attempt is not None
    assert retry_attempt.status == AttemptStatus.FAILED
    assert retry_attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert iteration.status == IterationStatus.FAILED
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    assert outcome.failure_summary == AttemptFailReason.STARTUP_FAILED.value
    assert [e.attempt_sequence_no for e in outcome.prior_attempt_history] == [1, 2]


def test_retry_start_failure_with_budget_remaining_creates_next_graph(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    """When budget remains after a startup failure on retry, the coordinator
    keeps trying until a non-failing factory or budget exhaustion."""
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=3)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        if attempt.attempt_sequence_no == 2:
            return _FailingStartOrchestrator(attempt.id)
        return _StartedOrchestrator(attempt.id, started)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )
    first_graph = coordinator.create_initial_attempt()
    attempt_store.close(
        first_graph.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    coordinator.handle_attempt_closed(first_graph.id)

    iteration = iteration_store.get(seg_id)
    assert iteration is not None
    assert len(iteration.attempt_ids) == 3
    g2 = attempt_store.get(iteration.attempt_ids[1])
    g3 = attempt_store.get(iteration.attempt_ids[2])
    assert g2 is not None and g2.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert g3 is not None and g3.status == AttemptStatus.RUNNING
    assert iteration.is_open
    assert captured == []


def test_failed_attempt_with_budget_starts_next_graph_orchestrator(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=2)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id, started)

    captured: list[IterationClosureReport] = []
    coordinator = IterationAttemptCoordinator(
        iteration_id=seg_id,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        on_iteration_closed=captured.append,
        orchestrator_factory=factory,
    )
    attempt = coordinator.create_initial_attempt()
    attempt_store.close(
        attempt.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    coordinator.handle_attempt_closed(attempt.id)

    iteration = iteration_store.get(seg_id)
    assert iteration is not None
    assert started == list(iteration.attempt_ids)
    assert captured == []


def test_failed_attempt_without_budget_emits_attempt_plan_failed(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=2)
    coordinator, captured = _make_coordinator(seg_id, iteration_store, attempt_store)
    g1 = coordinator.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id, plan_spec="spec1", evaluation_criteria=["a"], deferred_goal_for_next_iteration=None
    )
    attempt_store.close(
        g1.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g1.id)
    # second attempt
    seg = iteration_store.get(seg_id)
    assert seg is not None
    g2_id = seg.attempt_ids[-1]
    attempt_store.set_plan_contract(
        g2_id, plan_spec="spec2", evaluation_criteria=["b"], deferred_goal_for_next_iteration=None
    )
    attempt_store.close(
        g2_id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g2_id)
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    assert outcome.failure_summary == AttemptFailReason.EVALUATOR_FAILED.value


def test_prior_attempt_history_ordered_by_graph_sequence(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id, attempt_budget=2)
    coordinator, captured = _make_coordinator(seg_id, iteration_store, attempt_store)
    g1 = coordinator.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id, plan_spec="spec1", evaluation_criteria=["a"], deferred_goal_for_next_iteration=None
    )
    attempt_store.close(
        g1.id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g1.id)
    seg = iteration_store.get(seg_id)
    assert seg is not None
    g2_id = seg.attempt_ids[-1]
    attempt_store.set_plan_contract(
        g2_id, plan_spec="spec2", evaluation_criteria=["b"], deferred_goal_for_next_iteration=None
    )
    attempt_store.close(
        g2_id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g2_id)
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    seqs = [e.attempt_sequence_no for e in outcome.prior_attempt_history]
    assert seqs == [1, 2]


def test_creating_initial_graph_twice_raises(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    from task_center._core.primitives import TaskCenterInvariantViolation

    seg_id = _seed_segment(workflow_store, iteration_store, task_center_run_id)
    coordinator, _ = _make_coordinator(seg_id, iteration_store, attempt_store)
    coordinator.create_initial_attempt()
    with pytest.raises(TaskCenterInvariantViolation):
        coordinator.create_initial_attempt()

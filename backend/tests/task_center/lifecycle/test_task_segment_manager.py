"""TaskSegmentManager lifecycle tests."""

from __future__ import annotations

import pytest

from task_center.segment.manager import TaskSegmentManager
from task_center.harness_graph.graph import (
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.segment.closure_report import (
    AttemptPlanFailed,
    SuccessContinue,
    TaskSegmentClosureReport,
    TerminalSuccess,
)
from task_center.segment.segment import (
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)


def _seed_segment(
    request_store, segment_store, task_center_run_id, attempt_budget=2
) -> str:
    req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = segment_store.insert(
        complex_task_request_id=req.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=attempt_budget,
    )
    return seg.id


def _make_manager(seg_id, segment_store, graph_store):
    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
    )
    return mgr, captured


class _StartedOrchestrator:
    def __init__(self, graph_id: str, started: list[str]) -> None:
        self.harness_graph_id = graph_id
        self._started = started

    def start(self) -> None:
        self._started.append(self.harness_graph_id)


class _FailingStartOrchestrator:
    def __init__(self, graph_id: str) -> None:
        self.harness_graph_id = graph_id

    def start(self) -> None:
        raise RuntimeError("orchestrator start failed")


def test_initial_segment_creates_graph_sequence_1(
    request_store, segment_store, graph_store, task_center_run_id
):
    """Phase 01 exit: create segment 1 with harness graph sequence 1."""
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, segment_store, graph_store)
    g = mgr.create_initial_harness_graph()
    assert g.graph_sequence_no == 1
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.harness_graph_ids == (g.id,)


def test_retry_creates_graph_in_same_segment(
    request_store, segment_store, graph_store, task_center_run_id
):
    """Phase 01 exit: retry creates another HarnessGraph in the same segment."""
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, segment_store, graph_store)
    g1 = mgr.create_initial_harness_graph()
    g2 = mgr.create_next_harness_graph(previous_harness_graph_id=g1.id)
    assert g2.task_segment_id == seg_id
    assert g2.graph_sequence_no == 2
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.harness_graph_ids == (g1.id, g2.id)


def test_passing_graph_with_null_continuation_emits_terminal_success(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    mgr, captured = _make_manager(seg_id, segment_store, graph_store)
    g = mgr.create_initial_harness_graph()
    # No continuation_goal set on the graph.
    graph_store.close(
        g.id, status=HarnessGraphStatus.PASSED, fail_reason=None
    )
    mgr.handle_harness_graph_closed(g.id)
    assert len(captured) == 1
    assert isinstance(captured[0].outcome, TerminalSuccess)
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.status == TaskSegmentStatus.SUCCEEDED


def test_passing_graph_with_continuation_emits_success_continue(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    mgr, captured = _make_manager(seg_id, segment_store, graph_store)
    g = mgr.create_initial_harness_graph()
    graph_store.set_plan_contract(
        g.id,
        task_specification="spec",
        evaluation_criteria=["c1"],
        continuation_goal="next-goal",
    )
    graph_store.close(
        g.id, status=HarnessGraphStatus.PASSED, fail_reason=None
    )
    mgr.handle_harness_graph_closed(g.id)
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, SuccessContinue)
    assert outcome.goal == "next-goal"
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.continuation_goal == "next-goal"


def test_passing_graph_does_not_retry(
    request_store, segment_store, graph_store, task_center_run_id
):
    """Spec rule: passing graph always closes the segment; no second graph."""
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, segment_store, graph_store)
    g = mgr.create_initial_harness_graph()
    graph_store.close(
        g.id, status=HarnessGraphStatus.PASSED, fail_reason=None
    )
    mgr.handle_harness_graph_closed(g.id)
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.harness_graph_ids == (g.id,)
    assert seg.status == TaskSegmentStatus.SUCCEEDED


def test_failed_graph_with_budget_creates_next_graph(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, segment_store, graph_store)
    g1 = mgr.create_initial_harness_graph()
    graph_store.close(
        g1.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )
    mgr.handle_harness_graph_closed(g1.id)
    assert captured == []  # No closure report yet — segment still open.
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.is_open
    assert len(seg.harness_graph_ids) == 2


def test_failed_partial_plan_graph_retries_without_propagating_continuation(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, segment_store, graph_store)
    g1 = mgr.create_initial_harness_graph()
    graph_store.set_plan_contract(
        g1.id,
        task_specification="partial slice",
        evaluation_criteria=["slice passes"],
        continuation_goal="next slice",
    )
    graph_store.close(
        g1.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )

    mgr.handle_harness_graph_closed(g1.id)

    assert captured == []
    seg = segment_store.get(seg_id)
    assert seg is not None
    assert seg.is_open
    assert seg.continuation_goal is None
    assert len(seg.harness_graph_ids) == 2


def test_manager_starts_orchestrator_when_factory_present(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    started: list[str] = []

    def factory(graph, on_graph_closed):
        del on_graph_closed
        return _StartedOrchestrator(graph.id, started)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )

    graph = mgr.create_initial_harness_graph()

    assert started == [graph.id]


def test_initial_graph_start_can_be_deferred(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    started: list[str] = []

    def factory(graph, on_graph_closed):
        del on_graph_closed
        return _StartedOrchestrator(graph.id, started)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )

    graph = mgr.create_initial_harness_graph(start=False)
    assert started == []

    mgr.start_harness_graph(graph)

    assert started == [graph.id]


def test_initial_start_failure_closes_inserted_graph(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)

    def factory(graph, on_graph_closed):
        del on_graph_closed
        return _FailingStartOrchestrator(graph.id)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )

    with pytest.raises(RuntimeError, match="orchestrator start failed"):
        mgr.create_initial_harness_graph()

    segment = segment_store.get(seg_id)
    assert segment is not None
    assert len(segment.harness_graph_ids) == 1
    graph = graph_store.get(segment.harness_graph_ids[0])
    assert graph is not None
    assert graph.status == HarnessGraphStatus.FAILED
    assert graph.fail_reason == HarnessGraphFailReason.STARTUP_FAILED
    assert captured == []


def test_deferred_start_failure_closes_inserted_graph(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)

    def factory(graph, on_graph_closed):
        del on_graph_closed
        return _FailingStartOrchestrator(graph.id)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )

    graph = mgr.create_initial_harness_graph(start=False)

    with pytest.raises(RuntimeError, match="orchestrator start failed"):
        mgr.start_harness_graph(graph)

    latest = graph_store.get(graph.id)
    assert latest is not None
    assert latest.status == HarnessGraphStatus.FAILED
    assert latest.fail_reason == HarnessGraphFailReason.STARTUP_FAILED
    assert captured == []


def test_retry_start_failure_exhausts_budget_and_emits_closure(
    request_store, segment_store, graph_store, task_center_run_id
):
    """Retry-path startup failure closes the new graph STARTUP_FAILED and,
    when budget is exhausted, emits ``attempt_plan_failed`` instead of
    leaving the segment open."""
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=2)
    started: list[str] = []

    def factory(graph, on_graph_closed):
        del on_graph_closed
        if graph.graph_sequence_no == 1:
            return _StartedOrchestrator(graph.id, started)
        return _FailingStartOrchestrator(graph.id)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )
    first_graph = mgr.create_initial_harness_graph()
    graph_store.close(
        first_graph.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )

    mgr.handle_harness_graph_closed(first_graph.id)

    segment = segment_store.get(seg_id)
    assert segment is not None
    assert len(segment.harness_graph_ids) == 2
    retry_graph = graph_store.get(segment.harness_graph_ids[-1])
    assert retry_graph is not None
    assert retry_graph.status == HarnessGraphStatus.FAILED
    assert retry_graph.fail_reason == HarnessGraphFailReason.STARTUP_FAILED
    assert segment.status == TaskSegmentStatus.FAILED
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    assert outcome.failure_summary == HarnessGraphFailReason.STARTUP_FAILED.value
    assert [e.graph_sequence_no for e in outcome.attempted_plan_history] == [1, 2]


def test_retry_start_failure_with_budget_remaining_creates_next_graph(
    request_store, segment_store, graph_store, task_center_run_id
):
    """When budget remains after a startup failure on retry, the manager
    keeps trying until a non-failing factory or budget exhaustion."""
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=3)
    started: list[str] = []

    def factory(graph, on_graph_closed):
        del on_graph_closed
        if graph.graph_sequence_no == 2:
            return _FailingStartOrchestrator(graph.id)
        return _StartedOrchestrator(graph.id, started)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )
    first_graph = mgr.create_initial_harness_graph()
    graph_store.close(
        first_graph.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )

    mgr.handle_harness_graph_closed(first_graph.id)

    segment = segment_store.get(seg_id)
    assert segment is not None
    assert len(segment.harness_graph_ids) == 3
    g2 = graph_store.get(segment.harness_graph_ids[1])
    g3 = graph_store.get(segment.harness_graph_ids[2])
    assert g2 is not None and g2.fail_reason == HarnessGraphFailReason.STARTUP_FAILED
    assert g3 is not None and g3.status == HarnessGraphStatus.RUNNING
    assert segment.is_open
    assert captured == []


def test_failed_graph_with_budget_starts_next_graph_orchestrator(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=2)
    started: list[str] = []

    def factory(graph, on_graph_closed):
        del on_graph_closed
        return _StartedOrchestrator(graph.id, started)

    captured: list[TaskSegmentClosureReport] = []
    mgr = TaskSegmentManager(
        task_segment_id=seg_id,
        segment_store=segment_store,
        graph_store=graph_store,
        on_segment_closed=captured.append,
        orchestrator_factory=factory,
    )
    graph = mgr.create_initial_harness_graph()
    graph_store.close(
        graph.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )

    mgr.handle_harness_graph_closed(graph.id)

    segment = segment_store.get(seg_id)
    assert segment is not None
    assert started == list(segment.harness_graph_ids)
    assert captured == []


def test_failed_graph_without_budget_emits_attempt_plan_failed(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, segment_store, graph_store)
    g1 = mgr.create_initial_harness_graph()
    graph_store.set_plan_contract(
        g1.id, task_specification="spec1", evaluation_criteria=["a"], continuation_goal=None
    )
    graph_store.close(
        g1.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )
    mgr.handle_harness_graph_closed(g1.id)
    # second attempt
    seg = segment_store.get(seg_id)
    assert seg is not None
    g2_id = seg.harness_graph_ids[-1]
    graph_store.set_plan_contract(
        g2_id, task_specification="spec2", evaluation_criteria=["b"], continuation_goal=None
    )
    graph_store.close(
        g2_id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.EVALUATOR_FAILED,
    )
    mgr.handle_harness_graph_closed(g2_id)
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    assert outcome.failure_summary == HarnessGraphFailReason.EVALUATOR_FAILED.value


def test_attempted_plan_history_ordered_by_graph_sequence(
    request_store, segment_store, graph_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, segment_store, graph_store)
    g1 = mgr.create_initial_harness_graph()
    graph_store.set_plan_contract(
        g1.id, task_specification="spec1", evaluation_criteria=["a"], continuation_goal=None
    )
    graph_store.close(
        g1.id, status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )
    mgr.handle_harness_graph_closed(g1.id)
    seg = segment_store.get(seg_id)
    assert seg is not None
    g2_id = seg.harness_graph_ids[-1]
    graph_store.set_plan_contract(
        g2_id, task_specification="spec2", evaluation_criteria=["b"], continuation_goal=None
    )
    graph_store.close(
        g2_id, status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.EVALUATOR_FAILED,
    )
    mgr.handle_harness_graph_closed(g2_id)
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    seqs = [e.graph_sequence_no for e in outcome.attempted_plan_history]
    assert seqs == [1, 2]
    assert outcome.attempted_plan_history[0].harness_graph_summary_id is None
    assert outcome.attempted_plan_history[0].failure_landscape is None


def test_creating_initial_graph_twice_raises(
    request_store, segment_store, graph_store, task_center_run_id
):
    from task_center.exceptions import GraphInvariantViolation

    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, segment_store, graph_store)
    mgr.create_initial_harness_graph()
    with pytest.raises(GraphInvariantViolation):
        mgr.create_initial_harness_graph()

"""EpisodeManager lifecycle tests."""

from __future__ import annotations

import pytest

from task_center.episode.manager import EpisodeManager
from task_center.attempt import (
    AttemptFailReason,
    AttemptStatus,
)
from task_center.episode.state import (
    AttemptPlanFailed,
    SuccessContinue,
    EpisodeClosureReport,
    TerminalSuccess,
)
from task_center.episode.state import (
    EpisodeCreationReason,
    EpisodeStatus,
)


def _seed_segment(
    mission_store, episode_store, task_center_run_id, attempt_budget=2
) -> str:
    req = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = episode_store.insert(
        mission_id=req.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="g",
        attempt_budget=attempt_budget,
    )
    return seg.id


def _make_manager(seg_id, episode_store, attempt_store):
    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
    )
    return mgr, captured


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


def test_initial_episode_creates_graph_sequence_1(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    """Phase 01 exit: create episode 1 with harness attempt sequence 1."""
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, episode_store, attempt_store)
    g = mgr.create_initial_attempt()
    assert g.attempt_sequence_no == 1
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.attempt_ids == (g.id,)


def test_retry_creates_graph_in_same_segment(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    """Phase 01 exit: retry creates another Attempt in the same episode."""
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, episode_store, attempt_store)
    g1 = mgr.create_initial_attempt()
    g2 = mgr.create_next_attempt(previous_attempt_id=g1.id)
    assert g2.episode_id == seg_id
    assert g2.attempt_sequence_no == 2
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.attempt_ids == (g1.id, g2.id)


def test_passing_graph_with_null_continuation_emits_terminal_success(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    mgr, captured = _make_manager(seg_id, episode_store, attempt_store)
    g = mgr.create_initial_attempt()
    # No continuation_goal set on the attempt.
    attempt_store.close(
        g.id, status=AttemptStatus.PASSED, fail_reason=None
    )
    mgr.handle_attempt_closed(g.id)
    assert len(captured) == 1
    assert isinstance(captured[0].outcome, TerminalSuccess)
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.status == EpisodeStatus.SUCCEEDED


def test_passing_graph_with_continuation_emits_success_continue(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    mgr, captured = _make_manager(seg_id, episode_store, attempt_store)
    g = mgr.create_initial_attempt()
    attempt_store.set_plan_contract(
        g.id,
        task_specification="spec",
        evaluation_criteria=["c1"],
        continuation_goal="next-goal",
    )
    attempt_store.close(
        g.id, status=AttemptStatus.PASSED, fail_reason=None
    )
    mgr.handle_attempt_closed(g.id)
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, SuccessContinue)
    assert outcome.goal == "next-goal"
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.continuation_goal == "next-goal"


def test_passing_graph_does_not_retry(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    """Spec rule: passing attempt always closes the episode; no second attempt."""
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, episode_store, attempt_store)
    g = mgr.create_initial_attempt()
    attempt_store.close(
        g.id, status=AttemptStatus.PASSED, fail_reason=None
    )
    mgr.handle_attempt_closed(g.id)
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.attempt_ids == (g.id,)
    assert seg.status == EpisodeStatus.SUCCEEDED


def test_failed_attempt_with_budget_creates_next_graph(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, episode_store, attempt_store)
    g1 = mgr.create_initial_attempt()
    attempt_store.close(
        g1.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    mgr.handle_attempt_closed(g1.id)
    assert captured == []  # No closure report yet — episode still open.
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.is_open
    assert len(seg.attempt_ids) == 2


def test_failed_partial_plan_graph_retries_without_propagating_continuation(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, episode_store, attempt_store)
    g1 = mgr.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id,
        task_specification="partial slice",
        evaluation_criteria=["slice passes"],
        continuation_goal="next slice",
    )
    attempt_store.close(
        g1.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    mgr.handle_attempt_closed(g1.id)

    assert captured == []
    seg = episode_store.get(seg_id)
    assert seg is not None
    assert seg.is_open
    assert seg.continuation_goal is None
    assert len(seg.attempt_ids) == 2


def test_manager_starts_orchestrator_when_factory_present(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id, started)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )

    attempt = mgr.create_initial_attempt()

    assert started == [attempt.id]


def test_initial_graph_start_can_be_deferred(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id, started)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )

    attempt = mgr.create_unstarted_initial_attempt()
    assert started == []

    mgr.start_attempt(attempt)

    assert started == [attempt.id]


def test_initial_start_failure_closes_inserted_graph(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _FailingStartOrchestrator(attempt.id)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )

    with pytest.raises(RuntimeError, match="orchestrator start failed"):
        mgr.create_initial_attempt()

    episode = episode_store.get(seg_id)
    assert episode is not None
    assert len(episode.attempt_ids) == 1
    attempt = attempt_store.get(episode.attempt_ids[0])
    assert attempt is not None
    assert attempt.status == AttemptStatus.FAILED
    assert attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert captured == []


def test_deferred_start_failure_closes_inserted_graph(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _FailingStartOrchestrator(attempt.id)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )

    attempt = mgr.create_unstarted_initial_attempt()

    with pytest.raises(RuntimeError, match="orchestrator start failed"):
        mgr.start_attempt(attempt)

    latest = attempt_store.get(attempt.id)
    assert latest is not None
    assert latest.status == AttemptStatus.FAILED
    assert latest.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert captured == []


def test_retry_start_failure_exhausts_budget_and_emits_closure(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    """Retry-path startup failure closes the new attempt STARTUP_FAILED and,
    when budget is exhausted, emits ``attempt_plan_failed`` instead of
    leaving the episode open."""
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=2)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        if attempt.attempt_sequence_no == 1:
            return _StartedOrchestrator(attempt.id, started)
        return _FailingStartOrchestrator(attempt.id)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )
    first_graph = mgr.create_initial_attempt()
    attempt_store.close(
        first_graph.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    mgr.handle_attempt_closed(first_graph.id)

    episode = episode_store.get(seg_id)
    assert episode is not None
    assert len(episode.attempt_ids) == 2
    retry_attempt = attempt_store.get(episode.attempt_ids[-1])
    assert retry_attempt is not None
    assert retry_attempt.status == AttemptStatus.FAILED
    assert retry_attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert episode.status == EpisodeStatus.FAILED
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    assert outcome.failure_summary == AttemptFailReason.STARTUP_FAILED.value
    assert [e.attempt_sequence_no for e in outcome.attempted_plan_history] == [1, 2]


def test_retry_start_failure_with_budget_remaining_creates_next_graph(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    """When budget remains after a startup failure on retry, the manager
    keeps trying until a non-failing factory or budget exhaustion."""
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=3)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        if attempt.attempt_sequence_no == 2:
            return _FailingStartOrchestrator(attempt.id)
        return _StartedOrchestrator(attempt.id, started)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )
    first_graph = mgr.create_initial_attempt()
    attempt_store.close(
        first_graph.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    mgr.handle_attempt_closed(first_graph.id)

    episode = episode_store.get(seg_id)
    assert episode is not None
    assert len(episode.attempt_ids) == 3
    g2 = attempt_store.get(episode.attempt_ids[1])
    g3 = attempt_store.get(episode.attempt_ids[2])
    assert g2 is not None and g2.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert g3 is not None and g3.status == AttemptStatus.RUNNING
    assert episode.is_open
    assert captured == []


def test_failed_attempt_with_budget_starts_next_graph_orchestrator(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=2)
    started: list[str] = []

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id, started)

    captured: list[EpisodeClosureReport] = []
    mgr = EpisodeManager(
        episode_id=seg_id,
        episode_store=episode_store,
        attempt_store=attempt_store,
        on_episode_closed=captured.append,
        orchestrator_factory=factory,
    )
    attempt = mgr.create_initial_attempt()
    attempt_store.close(
        attempt.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    mgr.handle_attempt_closed(attempt.id)

    episode = episode_store.get(seg_id)
    assert episode is not None
    assert started == list(episode.attempt_ids)
    assert captured == []


def test_failed_attempt_without_budget_emits_attempt_plan_failed(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, episode_store, attempt_store)
    g1 = mgr.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id, task_specification="spec1", evaluation_criteria=["a"], continuation_goal=None
    )
    attempt_store.close(
        g1.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    mgr.handle_attempt_closed(g1.id)
    # second attempt
    seg = episode_store.get(seg_id)
    assert seg is not None
    g2_id = seg.attempt_ids[-1]
    attempt_store.set_plan_contract(
        g2_id, task_specification="spec2", evaluation_criteria=["b"], continuation_goal=None
    )
    attempt_store.close(
        g2_id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    mgr.handle_attempt_closed(g2_id)
    assert len(captured) == 1
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    assert outcome.failure_summary == AttemptFailReason.EVALUATOR_FAILED.value


def test_attempted_plan_history_ordered_by_graph_sequence(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id, attempt_budget=2)
    mgr, captured = _make_manager(seg_id, episode_store, attempt_store)
    g1 = mgr.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id, task_specification="spec1", evaluation_criteria=["a"], continuation_goal=None
    )
    attempt_store.close(
        g1.id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    mgr.handle_attempt_closed(g1.id)
    seg = episode_store.get(seg_id)
    assert seg is not None
    g2_id = seg.attempt_ids[-1]
    attempt_store.set_plan_contract(
        g2_id, task_specification="spec2", evaluation_criteria=["b"], continuation_goal=None
    )
    attempt_store.close(
        g2_id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    mgr.handle_attempt_closed(g2_id)
    outcome = captured[0].outcome
    assert isinstance(outcome, AttemptPlanFailed)
    seqs = [e.attempt_sequence_no for e in outcome.attempted_plan_history]
    assert seqs == [1, 2]
    assert outcome.attempted_plan_history[0].attempt_summary_id is None
    assert outcome.attempted_plan_history[0].failure_landscape is None


def test_creating_initial_graph_twice_raises(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    from task_center._core.types import TaskCenterInvariantViolation

    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    mgr, _ = _make_manager(seg_id, episode_store, attempt_store)
    mgr.create_initial_attempt()
    with pytest.raises(TaskCenterInvariantViolation):
        mgr.create_initial_attempt()

"""Tests for EpisodeClosureReport variants and history shape."""

from __future__ import annotations

from task_center.attempt import AttemptFailReason
from task_center.episode.state import (
    AttemptedPlanEntry,
    AttemptPlanFailed,
    SuccessContinue,
    EpisodeClosureReport,
    TerminalSuccess,
)


def test_terminal_success_constructs():
    o = TerminalSuccess()
    assert o.kind == "terminal_success"


def test_success_continue_carries_goal():
    o = SuccessContinue(goal="next")
    assert o.kind == "success_continue"
    assert o.goal == "next"


def test_attempt_plan_failed_carries_history():
    e1 = AttemptedPlanEntry(
        attempt_id="g1",
        attempt_sequence_no=1,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
        attempt_summary_id=None,
        failure_landscape=None,
    )
    o = AttemptPlanFailed(failure_summary="bad", attempted_plan_history=(e1,))
    assert o.kind == "attempt_plan_failed"
    assert o.attempted_plan_history == (e1,)


def test_attempted_plan_history_orders_by_sequence_no():
    e1 = AttemptedPlanEntry(
        attempt_id="g1",
        attempt_sequence_no=1,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=None,
        attempt_summary_id=None,
        failure_landscape=None,
    )
    e2 = AttemptedPlanEntry(
        attempt_id="g2",
        attempt_sequence_no=2,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=None,
        attempt_summary_id=None,
        failure_landscape=None,
    )
    seqs = [e.attempt_sequence_no for e in (e1, e2)]
    assert seqs == sorted(seqs)


def test_phase06_summary_fields_default_to_none():
    """Phase 06 fills these. Phase 01 must surface them as ``None``, not absent."""
    e = AttemptedPlanEntry(
        attempt_id="g1",
        attempt_sequence_no=1,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=None,
        attempt_summary_id=None,
        failure_landscape=None,
    )
    assert e.attempt_summary_id is None
    assert e.failure_landscape is None


def test_closure_report_carries_outcome():
    rep = EpisodeClosureReport(
        episode_id="s1",
        final_attempt_id="g1",
        outcome=TerminalSuccess(),
    )
    assert rep.outcome.kind == "terminal_success"

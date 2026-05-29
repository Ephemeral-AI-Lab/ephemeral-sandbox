"""Tests for IterationClosureReport variants and history shape."""

from __future__ import annotations

from task_center.attempt import AttemptFailReason
from task_center.iteration.state import (
    FailedAttemptEntry,
    AttemptPlanFailed,
    SuccessDeferred,
    IterationClosureReport,
    TerminalSuccess,
)


def test_terminal_success_constructs():
    o = TerminalSuccess()
    assert o.kind == "terminal_success"


def test_success_deferred_carries_deferred_goal():
    o = SuccessDeferred(deferred_goal_for_next_iteration="next")
    assert o.kind == "success_deferred"
    assert o.deferred_goal_for_next_iteration == "next"


def test_attempt_plan_failed_carries_history():
    e1 = FailedAttemptEntry(
        attempt_id="g1",
        attempt_sequence_no=1,
        plan_spec=None,
        evaluation_criteria=(),
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    o = AttemptPlanFailed(failure_summary="bad", prior_attempt_history=(e1,))
    assert o.kind == "attempt_plan_failed"
    assert o.prior_attempt_history == (e1,)


def test_prior_attempt_history_orders_by_sequence_no():
    e1 = FailedAttemptEntry(
        attempt_id="g1",
        attempt_sequence_no=1,
        plan_spec=None,
        evaluation_criteria=(),
        fail_reason=None,
    )
    e2 = FailedAttemptEntry(
        attempt_id="g2",
        attempt_sequence_no=2,
        plan_spec=None,
        evaluation_criteria=(),
        fail_reason=None,
    )
    seqs = [e.attempt_sequence_no for e in (e1, e2)]
    assert seqs == sorted(seqs)


def test_closure_report_carries_outcome():
    rep = IterationClosureReport(
        iteration_id="s1",
        final_attempt_id="g1",
        outcome=TerminalSuccess(),
    )
    assert rep.outcome.kind == "terminal_success"

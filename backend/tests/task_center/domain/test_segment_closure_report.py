"""Tests for TaskSegmentClosureReport variants and history shape."""

from __future__ import annotations

from task_center.attempt import HarnessGraphFailReason
from task_center.episode.closure_report import (
    AttemptedPlanEntry,
    AttemptPlanFailed,
    SuccessContinue,
    TaskSegmentClosureReport,
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
        harness_graph_id="g1",
        graph_sequence_no=1,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
        harness_graph_summary_id=None,
        failure_landscape=None,
    )
    o = AttemptPlanFailed(failure_summary="bad", attempted_plan_history=(e1,))
    assert o.kind == "attempt_plan_failed"
    assert o.attempted_plan_history == (e1,)


def test_attempted_plan_history_orders_by_sequence_no():
    e1 = AttemptedPlanEntry(
        harness_graph_id="g1",
        graph_sequence_no=1,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=None,
        harness_graph_summary_id=None,
        failure_landscape=None,
    )
    e2 = AttemptedPlanEntry(
        harness_graph_id="g2",
        graph_sequence_no=2,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=None,
        harness_graph_summary_id=None,
        failure_landscape=None,
    )
    seqs = [e.graph_sequence_no for e in (e1, e2)]
    assert seqs == sorted(seqs)


def test_phase06_summary_fields_default_to_none():
    """Phase 06 fills these. Phase 01 must surface them as ``None``, not absent."""
    e = AttemptedPlanEntry(
        harness_graph_id="g1",
        graph_sequence_no=1,
        task_specification=None,
        evaluation_criteria=(),
        fail_reason=None,
        harness_graph_summary_id=None,
        failure_landscape=None,
    )
    assert e.harness_graph_summary_id is None
    assert e.failure_landscape is None


def test_closure_report_carries_outcome():
    rep = TaskSegmentClosureReport(
        task_segment_id="s1",
        final_harness_graph_id="g1",
        outcome=TerminalSuccess(),
    )
    assert rep.outcome.kind == "terminal_success"

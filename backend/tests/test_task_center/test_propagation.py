"""Unit tests for ``task_center.propagation.close_with_summary``."""

from __future__ import annotations

import pytest

from task_center import Status, Task, TaskCenterError
from task_center.propagation import close_with_summary


def _t(id_: str, *, role: str = "executor", closes_for: str | None = None,
       status: Status = Status.PENDING) -> Task:
    return Task(
        id=id_,
        role=role,  # type: ignore[arg-type]
        title=id_,
        spec="...",
        status=status,
        closes_for=closes_for,
    )


def test_chain_closes_all_four_in_walk_order() -> None:
    """Doc example: closure chain a -> b -> c -> d.

    a (executor,  closes_for=None)
    b (evaluator, closes_for=a)   <- submit_continue_work_handoff
    c (executor,  closes_for=b)   <- submit_*_plan_handoff
    d (evaluator, closes_for=c)   <- submit_task_completion(S)

    Closing d propagates summary S to c, b, a.
    """
    a = _t("a", role="executor", closes_for=None, status=Status.HANDOFF)
    b = _t("b", role="evaluator", closes_for="a", status=Status.HANDOFF)
    c = _t("c", role="executor", closes_for="b", status=Status.HANDOFF)
    d = _t("d", role="evaluator", closes_for="c", status=Status.RUNNING)
    tasks = {"a": a, "b": b, "c": c, "d": d}

    closed = close_with_summary(tasks, "d", "S")

    assert closed == ["d", "c", "b", "a"]
    for tid in ("a", "b", "c", "d"):
        assert tasks[tid].status is Status.DONE
        assert tasks[tid].summary == "S"


def test_isolated_close() -> None:
    """A standalone task with closes_for=None closes only itself."""
    t = _t("only", closes_for=None, status=Status.RUNNING)
    closed = close_with_summary({"only": t}, "only", "result")
    assert closed == ["only"]
    assert t.status is Status.DONE
    assert t.summary == "result"


def test_missing_id_raises() -> None:
    """A dangling closes_for pointer surfaces as a TaskCenterError."""
    leaf = _t("leaf", closes_for="ghost", status=Status.RUNNING)
    with pytest.raises(TaskCenterError, match="not in tasks map"):
        close_with_summary({"leaf": leaf}, "leaf", "x")


def test_handoff_status_flips_to_done() -> None:
    """Mid-chain HANDOFF tasks flip to DONE just like leaves."""
    parent = _t("p", role="executor", closes_for=None, status=Status.HANDOFF)
    child = _t("c", role="evaluator", closes_for="p", status=Status.RUNNING)
    tasks = {"p": parent, "c": child}
    close_with_summary(tasks, "c", "ok")
    assert parent.status is Status.DONE
    assert parent.summary == "ok"

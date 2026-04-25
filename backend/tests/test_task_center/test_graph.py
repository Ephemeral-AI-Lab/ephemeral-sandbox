"""Unit tests for ``task_center.graph.TaskGraph``."""

from __future__ import annotations

import pytest

from task_center import Status, Task, TaskCenterError
from task_center.graph import TaskGraph


def _t(id_: str, *, role: str = "executor", status: Status = Status.PENDING,
       needs: frozenset[str] | None = None,
       parent_id: str | None = None, closes_for: str | None = None,
       children: list[str] | None = None) -> Task:
    return Task(
        id=id_,
        role=role,  # type: ignore[arg-type]
        title=id_,
        spec="...",
        status=status,
        needs=needs or frozenset(),
        parent_id=parent_id,
        closes_for=closes_for,
        children=list(children or []),
    )


def test_add_and_get() -> None:
    g = TaskGraph()
    a = _t("a")
    g.add(a)
    assert g.get("a") is a


def test_add_duplicate_raises() -> None:
    g = TaskGraph()
    g.add(_t("a"))
    with pytest.raises(TaskCenterError, match="already in graph"):
        g.add(_t("a"))


def test_get_missing_raises() -> None:
    g = TaskGraph()
    with pytest.raises(TaskCenterError, match="not in graph"):
        g.get("ghost")


def test_ready_tasks_no_deps() -> None:
    g = TaskGraph()
    g.add(_t("a"))
    g.add(_t("b"))
    ready = g.ready_tasks()
    assert {t.id for t in ready} == {"a", "b"}


def test_ready_tasks_waits_on_unsatisfied_deps() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.RUNNING))
    g.add(_t("b", needs=frozenset({"a"})))  # blocked on a
    ready = g.ready_tasks()
    assert ready == []


def test_ready_tasks_promotes_when_deps_done() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.DONE))
    g.add(_t("b", needs=frozenset({"a"})))
    ready = g.ready_tasks()
    assert {t.id for t in ready} == {"b"}


def test_transition_legal_move() -> None:
    g = TaskGraph()
    g.add(_t("a"))
    g.transition("a", Status.READY)
    assert g.get("a").status is Status.READY
    g.transition("a", Status.RUNNING)
    g.transition("a", Status.AWAITING)
    assert g.get("a").status is Status.AWAITING


def test_transition_rejects_pending_to_done() -> None:
    g = TaskGraph()
    g.add(_t("a"))
    with pytest.raises(ValueError, match="illegal transition"):
        g.transition("a", Status.DONE)


def test_transition_rejects_awaiting_to_done() -> None:
    """Invariant 14: AWAITING can only close via summary propagation."""
    g = TaskGraph()
    g.add(_t("a", status=Status.AWAITING))
    with pytest.raises(ValueError, match="illegal transition"):
        g.transition("a", Status.DONE)


def test_transition_rejects_done_to_anything() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.DONE))
    with pytest.raises(ValueError, match="illegal transition"):
        g.transition("a", Status.RUNNING)


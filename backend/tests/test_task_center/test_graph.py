"""Unit tests for ``task_center.graph.TaskGraph``."""

from __future__ import annotations

import pytest

from task_center import HarnessGraph, Status, Task, TaskCenterError
from task_center.graph import TaskGraph


def _t(
    id_: str,
    *,
    role: str = "executor",
    status: Status = Status.PENDING,
    needs: frozenset[str] | None = None,
    graph_id: str | None = None,
) -> Task:
    return Task(
        id=id_,
        role=role,  # type: ignore[arg-type]
        input="...",
        status=status,
        needs=needs or frozenset(),
        task_center_harness_graph_id=graph_id,
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
    assert {t.id for t in g.ready_tasks()} == {"a", "b"}


def test_ready_tasks_waits_on_unsatisfied_deps() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.RUNNING))
    g.add(_t("b", needs=frozenset({"a"})))
    assert g.ready_tasks() == []


def test_ready_tasks_promotes_when_deps_done() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.DONE))
    g.add(_t("b", needs=frozenset({"a"})))
    assert {t.id for t in g.ready_tasks()} == {"b"}


def test_transition_legal_move() -> None:
    g = TaskGraph()
    g.add(_t("a"))
    g.transition("a", Status.READY)
    g.transition("a", Status.RUNNING)
    g.transition("a", Status.HANDOFF)
    assert g.get("a").status is Status.HANDOFF


def test_handoff_can_close_to_done() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.HANDOFF))
    g.transition("a", Status.DONE)
    assert g.get("a").status is Status.DONE


def test_transition_rejects_pending_to_done() -> None:
    g = TaskGraph()
    g.add(_t("a"))
    with pytest.raises(ValueError, match="illegal transition"):
        g.transition("a", Status.DONE)


def test_transition_rejects_done_to_anything() -> None:
    g = TaskGraph()
    g.add(_t("a", status=Status.DONE))
    with pytest.raises(ValueError, match="illegal transition"):
        g.transition("a", Status.RUNNING)


def test_harness_graph_storage() -> None:
    g = TaskGraph()
    harness = HarnessGraph(
        id="g1",
        run_id="r1",
        root_task_id="caller",
        planner_task_id="planner",
    )
    g.add_harness_graph(harness)
    assert g.get_harness_graph("g1") is harness


def test_add_harness_graph_duplicate_raises() -> None:
    g = TaskGraph()
    h = HarnessGraph(
        id="g1", run_id="r", root_task_id="p", planner_task_id="pl"
    )
    g.add_harness_graph(h)
    with pytest.raises(TaskCenterError, match="already in graph"):
        g.add_harness_graph(h)

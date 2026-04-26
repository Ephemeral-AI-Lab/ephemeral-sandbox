"""Unit tests for ``task_center.task`` — Task dataclass + Status enum."""

from __future__ import annotations

import pytest

from task_center import (
    PlanValidationError,
    Status,
    Task,
    TaskCenterError,
)


def test_status_enum_has_exactly_six_values() -> None:
    expected = ["pending", "ready", "running", "handoff", "done", "failed"]
    assert [s.value for s in Status] == expected


def test_status_string_membership() -> None:
    assert Status.PENDING == "pending"
    assert Status.HANDOFF == "handoff"
    assert Status.DONE.value == "done"


def test_task_constructs_with_minimum_fields() -> None:
    task = Task(
        id="t1",
        role="executor",
        title="Trivial",
        spec="Do the thing.",
        status=Status.READY,
    )
    assert task.id == "t1"
    assert task.role == "executor"
    assert task.status is Status.READY
    assert task.parent_id is None
    assert task.closes_for is None
    assert task.needs == frozenset()
    assert task.acceptance_criteria is None
    assert task.handoff_note is None
    assert task.summary is None
    assert task.children == []
    assert task.evaluator_id is None
    assert isinstance(task.created_at, float)


def test_task_mutable_defaults_are_independent() -> None:
    a = Task(id="a", role="executor", title="A", spec="...", status=Status.PENDING)
    b = Task(id="b", role="executor", title="B", spec="...", status=Status.PENDING)
    a.children.append("child-of-a")
    assert b.children == []
    assert a.children == ["child-of-a"]


def test_closes_for_is_set_once_at_creation_to_none() -> None:
    task = Task(
        id="t",
        role="evaluator",
        title="Eval",
        spec="...",
        status=Status.PENDING,
        closes_for=None,
    )
    with pytest.raises(AttributeError, match="closes_for"):
        task.closes_for = "some-other-id"


def test_closes_for_is_set_once_at_creation_to_value() -> None:
    task = Task(
        id="t",
        role="evaluator",
        title="Eval",
        spec="...",
        status=Status.PENDING,
        closes_for="parent",
    )
    with pytest.raises(AttributeError, match="closes_for"):
        task.closes_for = "different-parent"


def test_closes_for_self_assign_is_a_no_op() -> None:
    task = Task(
        id="t",
        role="evaluator",
        title="Eval",
        spec="...",
        status=Status.PENDING,
        closes_for="parent",
    )
    task.closes_for = "parent"
    assert task.closes_for == "parent"


def test_other_fields_remain_mutable() -> None:
    task = Task(
        id="t",
        role="executor",
        title="T",
        spec="...",
        status=Status.PENDING,
    )
    task.status = Status.READY
    task.summary = "done"
    task.children.append("c1")
    assert task.status is Status.READY
    assert task.summary == "done"
    assert task.children == ["c1"]


def test_error_hierarchy() -> None:
    assert issubclass(PlanValidationError, TaskCenterError)
    assert issubclass(TaskCenterError, Exception)

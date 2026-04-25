"""Unit tests for ``task_center.dag.compile_dag``."""

from __future__ import annotations

import pytest

from task_center import PlanValidationError
from task_center.dag import compile_dag


def _specs(*ids: str) -> dict[str, dict[str, str]]:
    return {tid: {"title": tid, "spec": "..."} for tid in ids}


def test_rejects_bare_strings() -> None:
    with pytest.raises(PlanValidationError, match="entries must be objects"):
        compile_dag(["A"], _specs("A"))


def test_rejects_unknown_id() -> None:
    with pytest.raises(PlanValidationError, match="not a key in task_specs"):
        compile_dag([{"id": "X"}], _specs("A"))


def test_rejects_duplicate_id() -> None:
    with pytest.raises(PlanValidationError, match="duplicate task id"):
        compile_dag([{"id": "A"}, {"id": "A"}], _specs("A"))


def test_rejects_empty_tasks_list() -> None:
    with pytest.raises(PlanValidationError, match="tasks must be a non-empty list"):
        compile_dag([], _specs("A"))


def test_rejects_empty_task_specs() -> None:
    with pytest.raises(PlanValidationError, match="task_specs must be a non-empty dict"):
        compile_dag([{"id": "A"}], {})


def test_rejects_self_reference_in_deps() -> None:
    with pytest.raises(PlanValidationError, match="entry's own id"):
        compile_dag([{"id": "A", "deps": ["A"]}], _specs("A"))


def test_rejects_duplicates_in_deps() -> None:
    with pytest.raises(PlanValidationError, match="duplicate ids"):
        compile_dag(
            [{"id": "A"}, {"id": "B", "deps": ["A", "A"]}],
            _specs("A", "B"),
        )


def test_rejects_unknown_dep_reference() -> None:
    with pytest.raises(PlanValidationError, match="references unknown id"):
        compile_dag(
            [{"id": "A"}, {"id": "B", "deps": ["GHOST"]}],
            _specs("A", "B"),
        )


def test_deps_must_be_list() -> None:
    with pytest.raises(PlanValidationError, match="'deps' must be a list"):
        compile_dag(
            [{"id": "A"}, {"id": "B", "deps": "A"}],
            _specs("A", "B"),
        )


def test_rejects_self_cycle_via_two_nodes() -> None:
    with pytest.raises(PlanValidationError, match="cycle detected"):
        compile_dag(
            [
                {"id": "A", "deps": ["B"]},
                {"id": "B", "deps": ["A"]},
            ],
            _specs("A", "B"),
        )


def test_rejects_three_node_cycle() -> None:
    with pytest.raises(PlanValidationError, match="cycle detected"):
        compile_dag(
            [
                {"id": "A", "deps": ["C"]},
                {"id": "B", "deps": ["A"]},
                {"id": "C", "deps": ["B"]},
            ],
            _specs("A", "B", "C"),
        )


def test_simple_chain_compiles() -> None:
    deps = compile_dag(
        [
            {"id": "A"},
            {"id": "B", "deps": ["A"]},
            {"id": "C", "deps": ["B"]},
        ],
        _specs("A", "B", "C"),
    )
    assert deps["A"] == frozenset()
    assert deps["B"] == frozenset({"A"})
    assert deps["C"] == frozenset({"B"})


def test_diamond_compiles() -> None:
    deps = compile_dag(
        [
            {"id": "A"},
            {"id": "B", "deps": ["A"]},
            {"id": "C", "deps": ["A"]},
            {"id": "D", "deps": ["B", "C"]},
        ],
        _specs("A", "B", "C", "D"),
    )
    assert deps["D"] == frozenset({"B", "C"})


def test_omitted_deps_means_no_deps() -> None:
    deps = compile_dag([{"id": "A"}], _specs("A"))
    assert deps["A"] == frozenset()


def test_explicit_empty_deps_means_no_deps() -> None:
    deps = compile_dag([{"id": "A", "deps": []}], _specs("A"))
    assert deps["A"] == frozenset()



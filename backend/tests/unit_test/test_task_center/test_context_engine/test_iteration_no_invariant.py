"""iteration_no invariant on the ``<iteration_goal>`` child.

For every ``ContextBlock`` where ``metadata["iteration_no"]`` is set AND
``metadata["group_attrs"]`` contains ``iteration_no="``, the two integers
agree. Both derive from the same ``Iteration.sequence_no`` in the same
``ContextBlock(...)`` constructor in
``recipes/iterations.py:_current_iteration_goal_child``, so drift
is impossible by construction. This test pins the pairing — if a future
refactor splits the construction across two sites, the test fails before
the captures do.
"""

from __future__ import annotations

import re

import pytest

from task_center.context_engine.packet import ContextBlock
from task_center.context_engine.recipes.iterations import (
    _current_iteration_goal_child,
    goal_iteration_blocks,
)


class _FakeIteration:
    def __init__(
        self,
        *,
        id: str,
        sequence_no: int,
        goal: str,
        plan_spec: str | None = None,
        task_summary: str | None = None,
    ):
        self.id = id
        self.sequence_no = sequence_no
        self.goal = goal
        self.plan_spec = plan_spec
        self.task_summary = task_summary


class _FakeGoal:
    def __init__(self, *, id: str, goal: str):
        self.id = id
        self.goal = goal


def _parse_iteration_no_from_group_attrs(group_attrs: str) -> int | None:
    match = re.search(r'iteration_no="(\d+)"', group_attrs)
    return int(match.group(1)) if match else None


def _assert_invariant(blocks: list[ContextBlock]) -> None:
    for block in blocks:
        meta_iteration_no = block.metadata.get("iteration_no")
        group_attrs = block.metadata.get("group_attrs", "")
        attr_iteration_no = _parse_iteration_no_from_group_attrs(group_attrs)
        if meta_iteration_no is None or attr_iteration_no is None:
            continue
        assert int(meta_iteration_no) == attr_iteration_no, (
            f"iteration_no drift on block kind={block.kind!r}: "
            f"metadata['iteration_no']={meta_iteration_no!r} vs "
            f"group_attrs={group_attrs!r}"
        )


def test_current_iteration_goal_child_pairs_metadata_and_group_attrs():
    """The ``<iteration_goal>`` child carries BOTH metadata and group_attrs
    iteration_no; they must agree."""
    block = _current_iteration_goal_child(
        _FakeIteration(id="i7", sequence_no=7, goal="iter 7")
    )
    assert block.metadata["iteration_no"] == "7"
    assert 'iteration_no="7"' in block.metadata["group_attrs"]
    _assert_invariant([block])


def test_iter1_iteration_goal_body_uses_identity_marker():
    """Iteration 1's ``<iteration_goal>`` collapses to the literal marker."""
    block = _current_iteration_goal_child(
        _FakeIteration(id="i1", sequence_no=1, goal="iter 1 goal")
    )
    assert block.text == "(identical to &lt;goal&gt;)"
    assert block.metadata["iteration_no"] == "1"


@pytest.mark.parametrize("seq_no", [1, 2, 3, 5, 12, 99])
def test_iteration_no_invariant_holds_for_every_sequence_no(seq_no: int):
    block = _current_iteration_goal_child(
        _FakeIteration(id="i", sequence_no=seq_no, goal="g")
    )
    _assert_invariant([block])


def test_invariant_catches_planted_drift():
    """A hand-mutated block with mismatched fields must trip the invariant."""
    block = _current_iteration_goal_child(
        _FakeIteration(id="i", sequence_no=3, goal="g")
    )
    bad_metadata = dict(block.metadata)
    bad_metadata["iteration_no"] = "9999"
    bad = block.model_copy(update={"metadata": bad_metadata})
    with pytest.raises(AssertionError, match="iteration_no drift"):
        _assert_invariant([bad])


def test_goal_iteration_blocks_full_frame_invariant():
    """The full Iteration N≥2 frame: standalone ``<goal>`` + prior iteration
    groups + current iteration goal."""
    goal = _FakeGoal(id="g", goal="overall goal")
    prior = _FakeIteration(
        id="i1",
        sequence_no=1,
        goal="iter 1 goal",
        plan_spec="prior plan",
        task_summary="prior summary",
    )
    current = _FakeIteration(id="i2", sequence_no=2, goal="iter 2 goal")
    blocks = goal_iteration_blocks(
        goal=goal,
        current_iteration=current,
        iterations=[prior, current],
    )
    _assert_invariant(blocks)

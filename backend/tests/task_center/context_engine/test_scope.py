"""US-004: ContextScope.assert_fields behavior."""

from __future__ import annotations

import pytest

from task_center.context_engine.errors import RecipeScopeError
from task_center.context_engine.scope import ContextScope


def test_assert_fields_passes_when_all_present():
    scope = ContextScope(
        request_id="r",
        segment_id="s",
        harness_graph_id="g",
        task_id="t",
    )
    scope.assert_fields(frozenset({"request_id", "segment_id", "harness_graph_id"}))


def test_assert_fields_rejects_missing_segment():
    scope = ContextScope(request_id="r")
    with pytest.raises(RecipeScopeError) as exc:
        scope.assert_fields(frozenset({"request_id", "segment_id"}))
    assert "segment_id" in str(exc.value)


def test_assert_fields_lists_all_missing_fields_sorted():
    scope = ContextScope(request_id="r")
    with pytest.raises(RecipeScopeError) as exc:
        scope.assert_fields(
            frozenset({"task_id", "segment_id", "harness_graph_id"})
        )
    # Sorted output for deterministic error messages.
    msg = str(exc.value)
    assert "harness_graph_id" in msg
    assert "segment_id" in msg
    assert "task_id" in msg
    # Check sorted ordering.
    assert msg.index("harness_graph_id") < msg.index("segment_id")
    assert msg.index("segment_id") < msg.index("task_id")


def test_helper_scope_fields_round_trip():
    scope = ContextScope(
        request_id="r",
        task_id="helper-1",
        parent_packet_id="pkt-1",
        parent_task_id="parent-task",
    )
    scope.assert_fields(
        frozenset(
            {"request_id", "task_id", "parent_packet_id", "parent_task_id"}
        )
    )

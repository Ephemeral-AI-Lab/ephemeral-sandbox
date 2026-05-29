"""US-004: ContextScope.assert_fields behavior."""

from __future__ import annotations

import pytest

from task_center.context_engine.core import RecipeScopeError
from task_center.context_engine.scope import ContextScope


def test_assert_fields_passes_when_all_present():
    scope = ContextScope(
        workflow_id="r",
        iteration_id="s",
        attempt_id="g",
        task_id="t",
    )
    scope.assert_fields(frozenset({"workflow_id", "iteration_id", "attempt_id"}))


def test_assert_fields_rejects_missing_iteration():
    scope = ContextScope(workflow_id="r")
    with pytest.raises(RecipeScopeError) as exc:
        scope.assert_fields(frozenset({"workflow_id", "iteration_id"}))
    assert "iteration_id" in str(exc.value)


def test_assert_fields_lists_all_missing_fields_sorted():
    scope = ContextScope(workflow_id="r")
    with pytest.raises(RecipeScopeError) as exc:
        scope.assert_fields(
            frozenset({"task_id", "iteration_id", "attempt_id"})
        )
    # Sorted output for deterministic error messages.
    msg = str(exc.value)
    assert "iteration_id" in msg
    assert "task_id" in msg
    assert "attempt_id" in msg
    # Check sorted ordering.
    assert msg.index("attempt_id") < msg.index("iteration_id")
    assert msg.index("iteration_id") < msg.index("task_id")



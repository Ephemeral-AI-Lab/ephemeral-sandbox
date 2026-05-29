"""Phase 5j regression test - Saga inlining (lever #4).

After inlining task_center.saga.Saga directly into WorkflowStarter's
_compensate_failed_start method, this test pins:

1. The Saga module and its shim are gone.
2. _compensate_failed_start signature preserved (5 kwargs).
3. Best-effort semantics intact: failure in an early step does not
   block subsequent steps.

Plan: .omc/plans/task-center-folder-reframe-20260514.md (lever #4)
"""

from __future__ import annotations

import importlib
import inspect

import pytest

from task_center.workflow.starter import WorkflowStarter


def test_saga_module_is_gone() -> None:
    for path in ("task_center.saga", "task_center.workflow.saga"):
        with pytest.raises(ModuleNotFoundError):
            importlib.import_module(path)


def test_compensate_failed_start_signature_preserved() -> None:
    sig = inspect.signature(WorkflowStarter._compensate_failed_start)
    expected = {
        "self",
        "goal",
        "iteration",
        "initial_attempt_id",
        "origin",
    }
    assert set(sig.parameters) == expected


def test_inline_helper_uses_logger_not_saga() -> None:
    # Sanity: the method body must no longer reference Saga symbols.
    src = inspect.getsource(WorkflowStarter._compensate_failed_start)
    assert "Saga" not in src
    assert "SagaResult" not in src
    # Best-effort logging still emits per failed step.
    assert "logger.exception" in src

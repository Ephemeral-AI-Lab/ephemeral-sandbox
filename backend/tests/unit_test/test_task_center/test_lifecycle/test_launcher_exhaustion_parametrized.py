"""Phase 5d regression test - launcher exhaustion parameterization (lever #6).

After collapsing 4 _report_*_exhaustion functions + role dispatch table
into one _report_exhaustion(role-parametrized) function, this test pins:

1. The role dispatch table _ROLE_EXHAUSTION_REPORTERS is gone.
2. The 4 per-role _report_* helper functions are gone.
3. The single _report_exhaustion is callable + raises on unknown role.

Plan: .omc/plans/task-center-folder-reframe-20260514.md (lever #6)
"""

from __future__ import annotations

import pytest

from task_center.attempt import launch as launcher_module
from task_center._core.types import TaskCenterInvariantViolation


def test_role_dispatch_table_is_gone() -> None:
    assert not hasattr(launcher_module, "_ROLE_EXHAUSTION_REPORTERS")


def test_per_role_reporter_helpers_are_gone() -> None:
    for name in (
        "_report_entry_exhaustion",
        "_report_planner_exhaustion",
        "_report_generator_exhaustion",
        "_report_evaluator_exhaustion",
    ):
        assert not hasattr(launcher_module, name), name


def test_unified_report_exhaustion_exists() -> None:
    assert hasattr(launcher_module, "_report_exhaustion")
    assert callable(launcher_module._report_exhaustion)


def test_report_exhaustion_unknown_role_raises() -> None:
    # The function's final else-branch guards against future role enum
    # additions that lack a branch.
    from unittest.mock import MagicMock

    fake_role = MagicMock()
    fake_role.__repr__ = lambda self: "<fake-role>"  # type: ignore[method-assign]
    fake_launch = MagicMock()
    fake_launch.role = fake_role
    fake_launch.attempt_id = "a1"
    fake_launch.task_id = "t1"

    # Stub _require_attempt_orchestrator to return a non-None orchestrator
    # so we reach the role-branch tree.
    fake_orchestrator = MagicMock()
    orig = launcher_module._require_attempt_orchestrator
    try:
        launcher_module._require_attempt_orchestrator = (
            lambda *a, **k: fake_orchestrator
        )
        with pytest.raises(TaskCenterInvariantViolation):
            launcher_module._report_exhaustion(
                MagicMock(), MagicMock(), fake_launch, summary="boom"
            )
    finally:
        launcher_module._require_attempt_orchestrator = orig

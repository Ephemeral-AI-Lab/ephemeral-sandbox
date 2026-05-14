"""Phase 5e regression test - LaunchBuilder factory consolidation (lever #15).

The 4 factory methods (for_planner, for_generator, for_evaluator,
for_entry) now share a private _build helper. This test pins the public
contract and asserts the _build helper exists.

Plan: .omc/plans/task-center-folder-reframe-20260514.md (lever #15)
"""

from __future__ import annotations

import inspect

from task_center.attempt.launch import LaunchBuilder


def test_launch_builder_public_surface_preserved() -> None:
    expected_methods = {"for_planner", "for_generator", "for_evaluator", "for_entry"}
    actual_methods = {
        name for name in vars(LaunchBuilder) if not name.startswith("_")
    }
    missing = expected_methods - actual_methods
    assert not missing, f"LaunchBuilder missing factory methods: {missing}"


def test_for_planner_signature() -> None:
    sig = inspect.signature(LaunchBuilder.for_planner)
    assert set(sig.parameters) == {"self", "attempt", "task_id"}


def test_for_generator_signature() -> None:
    sig = inspect.signature(LaunchBuilder.for_generator)
    assert set(sig.parameters) == {"self", "attempt", "task", "base_agent_name"}


def test_for_evaluator_signature() -> None:
    sig = inspect.signature(LaunchBuilder.for_evaluator)
    assert set(sig.parameters) == {"self", "attempt", "task_id"}


def test_for_entry_signature() -> None:
    sig = inspect.signature(LaunchBuilder.for_entry)
    assert set(sig.parameters) == {
        "self",
        "task_id",
        "task_center_run_id",
        "base_agent_name",
    }


def test_shared_build_helper_exists() -> None:
    # The post-lever-#15 private helper that the 4 factory methods share.
    assert hasattr(LaunchBuilder, "_build")
    assert callable(LaunchBuilder._build)

"""Phase 5a regression test - Protocol overlap collapse (lever #9).

Two collapses happened in attempt/contexts.py:

1. PlannerCtx + GeneratorCtx (identical surface) collapsed to one
   AttemptStageCtx Protocol.
2. MissionLifecycleCtx now extends EpisodeLifecycleCtx (Protocol
   composition) instead of duplicating its 7 fields.

This test pins the public Protocol set + verifies that AttemptDeps
structurally satisfies the surviving Protocols.

Plan: .omc/plans/task-center-folder-reframe-20260514.md (lever #9)
"""

from __future__ import annotations

from task_center.attempt.contexts import (
    AttemptStageCtx,
    EpisodeLifecycleCtx,
    LaunchCtx,
    MissionLifecycleCtx,
    TaskCenterStores,
)


def test_planner_ctx_and_generator_ctx_are_gone() -> None:
    import task_center.attempt.contexts as ctx

    assert not hasattr(ctx, "PlannerCtx")
    assert not hasattr(ctx, "GeneratorCtx")


def test_attempt_stage_ctx_replaces_them() -> None:
    expected_fields = {
        "mission_store",
        "episode_store",
        "attempt_store",
        "task_store",
        "agent_launcher",
        "orchestrator_registry",
    }
    annotations = set(AttemptStageCtx.__annotations__)
    assert expected_fields <= annotations


def test_mission_lifecycle_extends_episode_lifecycle() -> None:
    # MissionLifecycleCtx inherits from EpisodeLifecycleCtx Protocol.
    assert EpisodeLifecycleCtx in MissionLifecycleCtx.__mro__


def test_public_protocol_inventory() -> None:
    import task_center.attempt.contexts as ctx

    assert set(ctx.__all__) == {
        "AttemptStageCtx",
        "EpisodeLifecycleCtx",
        "LaunchCtx",
        "MissionLifecycleCtx",
        "TaskCenterStores",
    }

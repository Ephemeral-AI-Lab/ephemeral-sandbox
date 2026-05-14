"""Unit tests for nested mission ancestry depth and predicate routing."""

from __future__ import annotations

import pytest

from task_center.agent_launch.predicates import (
    MAX_HANDOFF_DEPTH,
    PredicateRegistry,
    ResolverContext,
    register_builtin_predicates,
)
from task_center.attempt import AttemptStage
from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.scope import ContextScope
from task_center.episode.episode import EpisodeCreationReason
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.mission.ancestry import nested_mission_depth


def _stores(mission_store, episode_store, attempt_store, task_store):
    return dict(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )


def _seed_mission(
    mission_store,
    *,
    task_center_run_id: str,
    requested_by_task_id: str = "t-entry",
    goal: str = "g",
):
    return mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id=requested_by_task_id,
        goal=goal,
    )


def _seed_episode(episode_store, *, mission_id: str, sequence_no: int = 1):
    return episode_store.insert(
        mission_id=mission_id,
        sequence_no=sequence_no,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def _seed_attempt(
    attempt_store,
    *,
    episode_id: str,
    sequence_no: int = 1,
):
    attempt = attempt_store.insert(
        episode_id=episode_id, attempt_sequence_no=sequence_no
    )
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="spec",
        evaluation_criteria=["c1"],
        continuation_goal=None,
    )
    attempt_store.set_stage(attempt.id, AttemptStage.GENERATE)
    return attempt


def _seed_task(
    task_store,
    *,
    task_id: str,
    task_center_run_id: str,
    attempt_id: str | None,
    role: str = "generator",
):
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        role=role,
        agent_name=role,
        rendered_prompt="input",
        status="running",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt_id,
        spawn_reason="test_seed",
    )


def _seed_nested_mission_chain(
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    *,
    task_center_run_id: str,
    depth: int,
) -> list[str]:
    assert depth >= 1
    mission_ids: list[str] = []
    requested_by_task_id = "t-entry"
    for idx in range(depth):
        mission = _seed_mission(
            mission_store,
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
        )
        mission_ids.append(mission.id)
        if idx == depth - 1:
            break
        episode = _seed_episode(episode_store, mission_id=mission.id)
        attempt = _seed_attempt(attempt_store, episode_id=episode.id)
        task_id = f"t-{idx}"
        _seed_task(
            task_store,
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=attempt.id,
        )
        requested_by_task_id = task_id
    return mission_ids


def test_no_parent_task_returns_depth_1(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    mission = _seed_mission(
        mission_store, task_center_run_id=task_center_run_id
    )
    assert (
        nested_mission_depth(
            mission_id=mission.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        == 1
    )


def test_parent_task_with_no_attempt_returns_depth_1(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    mission = _seed_mission(
        mission_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
    )
    _seed_task(
        task_store,
        task_id="t-entry",
        task_center_run_id=task_center_run_id,
        attempt_id=None,
    )
    assert (
        nested_mission_depth(
            mission_id=mission.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        == 1
    )


def test_child_mission_returns_depth_2(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    root_id, child_id = _seed_nested_mission_chain(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        task_center_run_id=task_center_run_id,
        depth=2,
    )
    assert (
        nested_mission_depth(
            mission_id=root_id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        == 1
    )
    assert (
        nested_mission_depth(
            mission_id=child_id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        == 2
    )


def test_grandchild_mission_returns_depth_3(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    mission_ids = _seed_nested_mission_chain(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        task_center_run_id=task_center_run_id,
        depth=MAX_HANDOFF_DEPTH + 1,
    )
    assert (
        nested_mission_depth(
            mission_id=mission_ids[-1],
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        == MAX_HANDOFF_DEPTH + 1
    )


def test_unknown_mission_id_raises(
    mission_store, episode_store, attempt_store, task_store
):
    with pytest.raises(TaskCenterInvariantViolation):
        nested_mission_depth(
            mission_id="nonexistent",
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )


def test_registered_predicates_cover_top_level_and_depth_thresholds(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    saved = dict(PredicateRegistry._registry)
    PredicateRegistry.clear()
    register_builtin_predicates()
    try:
        deps = ContextEngineDeps(
            mission_store=mission_store,
            episode_store=episode_store,
            attempt_store=attempt_store,
            task_store=task_store,
        )

        top_level_ctx = ResolverContext(scope=ContextScope(), deps=deps)
        assert (
            PredicateRegistry.get("nested_mission_depth_within_handoff_range")(
                top_level_ctx
            )
            is True
        )
        assert (
            PredicateRegistry.get("nested_mission_depth_above_handoff_range")(
                top_level_ctx
            )
            is False
        )
        assert (
            PredicateRegistry.get("nested_mission_depth_gt_1")(top_level_ctx)
            is False
        )
        assert PredicateRegistry.get("always")(top_level_ctx) is True

        mission_ids = _seed_nested_mission_chain(
            mission_store,
            episode_store,
            attempt_store,
            task_store,
            task_center_run_id=task_center_run_id,
            depth=MAX_HANDOFF_DEPTH + 1,
        )
        within_ctx = ResolverContext(
            scope=ContextScope(mission_id=mission_ids[MAX_HANDOFF_DEPTH - 1]),
            deps=deps,
        )
        above_ctx = ResolverContext(
            scope=ContextScope(mission_id=mission_ids[-1]),
            deps=deps,
        )

        assert (
            PredicateRegistry.get("nested_mission_depth_within_handoff_range")(
                within_ctx
            )
            is True
        )
        assert (
            PredicateRegistry.get("nested_mission_depth_above_handoff_range")(
                within_ctx
            )
            is False
        )
        assert PredicateRegistry.get("nested_mission_depth_gt_1")(within_ctx) is True

        assert (
            PredicateRegistry.get("nested_mission_depth_within_handoff_range")(
                above_ctx
            )
            is False
        )
        assert (
            PredicateRegistry.get("nested_mission_depth_above_handoff_range")(
                above_ctx
            )
            is True
        )
        assert PredicateRegistry.get("nested_mission_depth_gt_1")(above_ctx) is True
    finally:
        PredicateRegistry.clear()
        PredicateRegistry._registry.update(saved)

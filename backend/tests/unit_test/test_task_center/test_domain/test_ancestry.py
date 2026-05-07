"""Unit tests for the canonical ancestry walker.

Pins walker behavior across no/full/partial caller chains and verifies the
registered resolver predicate returns the same result.
"""

from __future__ import annotations

import pytest

from task_center.agent_launch.predicates import (
    PredicateRegistry,
    register_builtin_predicates,
)
from task_center.mission.ancestry import (
    has_partial_planned_caller_ancestor,
)
from task_center.attempt import AttemptStage
from task_center.episode.episode import EpisodeCreationReason


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
    continuation_goal: str | None = None,
):
    attempt = attempt_store.insert(
        episode_id=episode_id, attempt_sequence_no=sequence_no
    )
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="spec",
        evaluation_criteria=["c1"],
        continuation_goal=continuation_goal,
    )
    attempt_store.set_stage(attempt.id, AttemptStage.GENERATING)
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
        task_input="input",
        status="running",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt_id,
        spawn_reason="test_seed",
    )


# ---------------------------------------------------------------------------
# Walker behavior
# ---------------------------------------------------------------------------


def test_no_parent_task_returns_false(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    request = _seed_mission(
        mission_store, task_center_run_id=task_center_run_id
    )
    # No parent task seeded → walk terminates returning False.
    assert (
        has_partial_planned_caller_ancestor(
            mission_id=request.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        is False
    )


def test_parent_task_with_no_attempt_returns_false(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    request = _seed_mission(
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
        has_partial_planned_caller_ancestor(
            mission_id=request.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        is False
    )


def test_full_plan_caller_chain_returns_false(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    # Top-level request → episode → caller_attempt (full plan: continuation_goal=None)
    parent_mission = _seed_mission(
        mission_store, task_center_run_id=task_center_run_id
    )
    parent_episode = _seed_episode(episode_store, mission_id=parent_mission.id)
    caller_attempt = _seed_attempt(
        attempt_store, episode_id=parent_episode.id, continuation_goal=None
    )
    _seed_task(
        task_store,
        task_id="t-caller",
        task_center_run_id=task_center_run_id,
        attempt_id=caller_attempt.id,
    )
    child_mission = _seed_mission(
        mission_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
    )
    assert (
        has_partial_planned_caller_ancestor(
            mission_id=child_mission.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        is False
    )


def test_partial_plan_caller_returns_true(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    parent_mission = _seed_mission(
        mission_store, task_center_run_id=task_center_run_id
    )
    parent_episode = _seed_episode(episode_store, mission_id=parent_mission.id)
    caller_attempt = _seed_attempt(
        attempt_store,
        episode_id=parent_episode.id,
        continuation_goal="continue here",
    )
    _seed_task(
        task_store,
        task_id="t-caller",
        task_center_run_id=task_center_run_id,
        attempt_id=caller_attempt.id,
    )
    child_mission = _seed_mission(
        mission_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
    )
    assert (
        has_partial_planned_caller_ancestor(
            mission_id=child_mission.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        is True
    )


def test_deep_mixed_chain_with_partial_root_returns_true(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    # Three-deep: root submits partial → child full → grandchild request.
    root_mission = _seed_mission(
        mission_store, task_center_run_id=task_center_run_id
    )
    root_episode = _seed_episode(episode_store, mission_id=root_mission.id)
    root_attempt = _seed_attempt(
        attempt_store,
        episode_id=root_episode.id,
        continuation_goal="rotate next",
    )
    _seed_task(
        task_store,
        task_id="t-root",
        task_center_run_id=task_center_run_id,
        attempt_id=root_attempt.id,
    )
    mid_mission = _seed_mission(
        mission_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-root",
    )
    mid_episode = _seed_episode(episode_store, mission_id=mid_mission.id)
    mid_attempt = _seed_attempt(
        attempt_store, episode_id=mid_episode.id, continuation_goal=None
    )
    _seed_task(
        task_store,
        task_id="t-mid",
        task_center_run_id=task_center_run_id,
        attempt_id=mid_attempt.id,
    )
    leaf_mission = _seed_mission(
        mission_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-mid",
    )

    assert (
        has_partial_planned_caller_ancestor(
            mission_id=leaf_mission.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        is True
    )


def test_unknown_mission_id_raises(
    mission_store, episode_store, attempt_store, task_store
):
    from task_center.exceptions import TaskCenterInvariantViolation

    with pytest.raises(TaskCenterInvariantViolation):
        has_partial_planned_caller_ancestor(
            mission_id="nonexistent",
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )


# ---------------------------------------------------------------------------
# Resolver predicate behavior
# ---------------------------------------------------------------------------


def test_resolver_predicate_dispatches_to_canonical(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    """The registered resolver predicate must call the canonical ancestry
    function — confirmed by checking the result matches the canonical's."""
    saved = dict(PredicateRegistry._registry)
    PredicateRegistry.clear()
    register_builtin_predicates()
    try:
        from task_center.context_engine.engine import ContextEngineDeps
        from task_center.agent_launch.predicates import ResolverContext
        from task_center.context_engine.scope import ContextScope

        # Seed a partial-plan caller chain.
        parent_mission = _seed_mission(
            mission_store, task_center_run_id=task_center_run_id
        )
        parent_episode = _seed_episode(episode_store, mission_id=parent_mission.id)
        caller_attempt = _seed_attempt(
            attempt_store,
            episode_id=parent_episode.id,
            continuation_goal="next",
        )
        _seed_task(
            task_store,
            task_id="t-caller",
            task_center_run_id=task_center_run_id,
            attempt_id=caller_attempt.id,
        )
        child_mission = _seed_mission(
            mission_store,
            task_center_run_id=task_center_run_id,
            requested_by_task_id="t-caller",
        )
        deps = ContextEngineDeps(
            mission_store=mission_store,
            episode_store=episode_store,
            attempt_store=attempt_store,
            task_store=task_store,
        )
        ctx = ResolverContext(
            scope=ContextScope(mission_id=child_mission.id), deps=deps
        )
        predicate = PredicateRegistry.get("partial_plan_caller_ancestor")
        canonical_result = has_partial_planned_caller_ancestor(
            mission_id=child_mission.id,
            **_stores(mission_store, episode_store, attempt_store, task_store),
        )
        assert predicate(ctx) is True
        assert predicate(ctx) is canonical_result, (
            "resolver predicate must yield the same answer as the canonical"
        )

    finally:
        PredicateRegistry.clear()
        PredicateRegistry._registry.update(saved)

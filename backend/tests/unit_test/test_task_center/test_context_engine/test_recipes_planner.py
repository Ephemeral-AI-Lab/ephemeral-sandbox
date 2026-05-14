"""US-010: planner_v1 block taxonomy and conditional logic."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import (
    ContextPriority,
)
from task_center.context_engine.recipes.planner import (
    _planner_v1_build,
)
from task_center.context_engine.scope import ContextScope
from task_center.attempt import (
    AttemptFailReason,
    AttemptStatus,
)
from task_center.episode.episode import (
    EpisodeCreationReason,
    EpisodeStatus,
)


@pytest.fixture
def deps_with_stores(
    mission_store, episode_store, attempt_store, task_store
) -> ContextEngineDeps:
    return ContextEngineDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )


def _seed_mission(mission_store, task_center_run_id, goal="goal"):
    return mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal=goal,
    )


def _seed_episode(
    episode_store,
    *,
    mission_id: str,
    sequence_no: int,
    goal: str = "g",
):
    return episode_store.insert(
        mission_id=mission_id,
        sequence_no=sequence_no,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal=goal,
        attempt_budget=2,
    )


def _close_episode_succeeded(
    episode_store, episode_id, *, spec: str, summary: str
):
    return episode_store.close_succeeded(
        episode_id,
        task_specification=spec,
        task_summary=summary,
        closed_at=datetime.now(UTC),
    )


def _seed_failed_attempt(attempt_store, episode_id, *, sequence_no: int):
    g = attempt_store.insert(
        episode_id=episode_id, attempt_sequence_no=sequence_no
    )
    attempt_store.set_plan_contract(
        g.id,
        task_specification=f"spec-{sequence_no}",
        evaluation_criteria=[f"crit-{sequence_no}-a", f"crit-{sequence_no}-b"],
        continuation_goal=None,
    )
    return attempt_store.close(
        g.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
        closed_at=datetime.now(UTC),
    )


def _seed_running_attempt(attempt_store, episode_id, *, sequence_no: int):
    return attempt_store.insert(
        episode_id=episode_id, attempt_sequence_no=sequence_no
    )


# ---------------------------------------------------------------------------
# episode-1 branch
# ---------------------------------------------------------------------------


def test_episode1_emits_one_merged_mission_episode_block(
    deps_with_stores, mission_store, episode_store, attempt_store,
    task_center_run_id,
):
    request = _seed_mission(mission_store, task_center_run_id, goal="overall")
    episode = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="overall"
    )
    g = _seed_running_attempt(attempt_store, episode.id, sequence_no=1)

    packet = _planner_v1_build(
        ContextScope(
            mission_id=request.id, episode_id=episode.id, attempt_id=g.id
        ),
        deps_with_stores,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["episode_goal"]
    episode_goal = packet.blocks[0]
    assert episode_goal.metadata["heading"] == "# Mission / Current Episode"
    assert packet.target_id == g.id


# ---------------------------------------------------------------------------
# episode-2 / episode-N branch
# ---------------------------------------------------------------------------


def test_episode2_emits_mission_prior_results_and_current_episode(
    deps_with_stores, mission_store, episode_store, attempt_store,
    task_center_run_id,
):
    request = _seed_mission(mission_store, task_center_run_id, goal="overall")
    episode1 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="episode1 goal"
    )
    _close_episode_succeeded(
        episode_store, episode1.id, spec="episode1 spec", summary="episode1 summary"
    )
    episode2 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=2, goal="episode2 goal"
    )
    g = _seed_running_attempt(attempt_store, episode2.id, sequence_no=1)

    packet = _planner_v1_build(
        ContextScope(
            mission_id=request.id, episode_id=episode2.id, attempt_id=g.id
        ),
        deps_with_stores,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == [
        "mission_goal",
        "prior_episode_specification",
        "prior_episode_summary",
        "episode_goal",
    ]
    assert packet.blocks[0].metadata["heading"] == "# Mission"
    prior_spec = packet.blocks[1]
    assert prior_spec.priority == ContextPriority.HIGH
    assert prior_spec.metadata["episode_sequence_no"] == "1"
    assert prior_spec.metadata["group_heading"] == "# Previous Episode Results"
    assert prior_spec.text == "episode1 spec"
    episode_goal = packet.blocks[3]
    assert episode_goal.metadata["heading"] == "# Current Episode"


def test_episode3_emits_two_pairs_with_priority_split(
    deps_with_stores, mission_store, episode_store, attempt_store,
    task_center_run_id,
):
    request = _seed_mission(mission_store, task_center_run_id, goal="overall")
    episode1 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="g1"
    )
    _close_episode_succeeded(episode_store, episode1.id, spec="s1", summary="sum1")
    episode2 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=2, goal="g2"
    )
    _close_episode_succeeded(episode_store, episode2.id, spec="s2", summary="sum2")
    episode3 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=3, goal="g3"
    )
    g = _seed_running_attempt(attempt_store, episode3.id, sequence_no=1)

    packet = _planner_v1_build(
        ContextScope(
            mission_id=request.id, episode_id=episode3.id, attempt_id=g.id
        ),
        deps_with_stores,
    )
    # Two prior episodes in sequence order; immediate prior is HIGH.
    prior_specs = [
        b for b in packet.blocks if b.kind == "prior_episode_specification"
    ]
    assert len(prior_specs) == 2
    assert prior_specs[0].metadata["episode_sequence_no"] == "1"
    assert prior_specs[0].priority == ContextPriority.MEDIUM
    assert prior_specs[1].metadata["episode_sequence_no"] == "2"
    assert prior_specs[1].priority == ContextPriority.HIGH


def test_missing_prior_spec_raises_context_engine_error(
    deps_with_stores, mission_store, episode_store, attempt_store,
    task_center_run_id,
):
    """Closed episode-1 with task_specification still null is an invariant
    violation; recipe must raise."""
    request = _seed_mission(mission_store, task_center_run_id)
    episode1 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="g1"
    )
    # Close via legacy set_status (does not write denormalized fields).
    episode_store.set_status(
        episode1.id, status=EpisodeStatus.SUCCEEDED, closed_at=datetime.now(UTC)
    )
    episode2 = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=2, goal="g2"
    )
    g = _seed_running_attempt(attempt_store, episode2.id, sequence_no=1)

    with pytest.raises(ContextEngineError):
        _planner_v1_build(
            ContextScope(
                mission_id=request.id, episode_id=episode2.id, attempt_id=g.id
            ),
            deps_with_stores,
        )


# ---------------------------------------------------------------------------
# Failed-attempt landscape blocks (current episode retries)
# ---------------------------------------------------------------------------


def test_three_failed_attempts_emit_three_high_priority_blocks(
    deps_with_stores, mission_store, episode_store, attempt_store,
    task_center_run_id,
):
    request = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="g"
    )
    for n in (1, 2, 3):
        _seed_failed_attempt(attempt_store, episode.id, sequence_no=n)
    current_attempt = _seed_running_attempt(attempt_store, episode.id, sequence_no=4)

    packet = _planner_v1_build(
        ContextScope(
            mission_id=request.id,
            episode_id=episode.id,
            attempt_id=current_attempt.id,
        ),
        deps_with_stores,
    )
    failed_blocks = [
        b for b in packet.blocks if b.kind == "failed_attempt_landscape"
    ]
    assert len(failed_blocks) == 3
    for block in failed_blocks:
        assert block.priority == ContextPriority.HIGH
    assert [b.metadata["attempt_sequence_no"] for b in failed_blocks] == [
        "1",
        "2",
        "3",
    ]


def test_failed_attempt_landscape_includes_plan_type_statuses_and_summaries(
    deps_with_stores, mission_store, episode_store, attempt_store, task_store,
    task_center_run_id,
):
    request = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="g"
    )
    failed = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        failed.id,
        task_specification="partial failed spec",
        evaluation_criteria=["criterion"],
        continuation_goal="continue with later slice",
    )
    attempt_store.set_generator_task_ids(failed.id, ["gen-a", "gen-b"])
    task_store.upsert_task(
        task_id="gen-a",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="a",
        status="done",
        summaries=[{"summary": "implemented A"}],
        needs=[],
        task_center_attempt_id=failed.id,
        spawn_reason="attempt_generator",
    )
    task_store.upsert_task(
        task_id="gen-b",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="b",
        status="failed",
        summaries=[{"summary": "B failed after creating fixture"}],
        needs=[],
        task_center_attempt_id=failed.id,
        spawn_reason="attempt_generator",
    )
    attempt_store.close(
        failed.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
        closed_at=datetime.now(UTC),
    )
    current_attempt = _seed_running_attempt(attempt_store, episode.id, sequence_no=2)

    packet = _planner_v1_build(
        ContextScope(
            mission_id=request.id,
            episode_id=episode.id,
            attempt_id=current_attempt.id,
        ),
        deps_with_stores,
    )

    failed_blocks = [
        b for b in packet.blocks if b.kind == "failed_attempt_landscape"
    ]
    assert len(failed_blocks) == 1
    text = failed_blocks[0].text
    assert "Plan type: partial" in text
    assert "continue with later slice" not in text
    assert "- gen-a: done" in text
    assert "- gen-b: failed" in text
    assert "#### gen-a\n\nimplemented A" in text
    assert "#### gen-b\n\nB failed after creating fixture" in text
    assert "fail_reason" not in text


def test_all_failed_attempts_render_as_high_priority_blocks(
    deps_with_stores, mission_store, episode_store, attempt_store,
    task_center_run_id,
):
    request = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(
        episode_store, mission_id=request.id, sequence_no=1, goal="g"
    )
    total = 8
    for n in range(1, total + 1):
        _seed_failed_attempt(attempt_store, episode.id, sequence_no=n)
    current_attempt = _seed_running_attempt(
        attempt_store, episode.id, sequence_no=total + 1
    )

    packet = _planner_v1_build(
        ContextScope(
            mission_id=request.id,
            episode_id=episode.id,
            attempt_id=current_attempt.id,
        ),
        deps_with_stores,
    )
    failed_blocks = [
        b for b in packet.blocks if b.kind == "failed_attempt_landscape"
    ]
    assert len(failed_blocks) == total
    assert [b.metadata["attempt_sequence_no"] for b in failed_blocks] == [
        str(n) for n in range(1, total + 1)
    ]
    assert all(block.priority == ContextPriority.HIGH for block in failed_blocks)
    assert all("truncated_count" not in block.metadata for block in failed_blocks)

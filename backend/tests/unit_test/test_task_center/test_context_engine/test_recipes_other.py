"""US-010: generator_v1, evaluator_v1, entry_executor_v1 happy-path."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import ContextPriority
from task_center.context_engine.recipes.entry_executor import (
    _entry_executor_v1_build,
)
from task_center.context_engine.recipes.evaluator import _evaluator_v1_build
from task_center.context_engine.recipes.generator import _generator_v1_build
from task_center.context_engine.scope import ContextScope
from task_center.episode.episode import EpisodeCreationReason


@pytest.fixture
def deps(
    mission_store, episode_store, attempt_store, task_store
) -> ContextEngineDeps:
    return ContextEngineDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )


def _seed_mission(mission_store, task_center_run_id):
    return mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal="overall",
    )


def _seed_episode(episode_store, *, mission_id):
    return episode_store.insert(
        mission_id=mission_id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def _seed_continuation_episode(episode_store, *, mission_id):
    return episode_store.insert(
        mission_id=mission_id,
        sequence_no=2,
        creation_reason=EpisodeCreationReason.PARTIAL_CONTINUATION,
        goal="g2",
        attempt_budget=2,
    )


# ---------------------------------------------------------------------------
# generator_v1
# ---------------------------------------------------------------------------


def test_generator_v1_emits_planned_task_spec_required_block(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="attempt spec framing",
        evaluation_criteria=["c1"],
        continuation_goal=None,
    )
    task_id = "t-1"
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="do thing X",
        status="pending",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )
    packet = _generator_v1_build(
        ContextScope(
            mission_id=req.id,
            attempt_id=attempt.id,
            task_id=task_id,
        ),
        deps,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["task_specification", "planned_task_spec"]
    assert packet.blocks[-1].priority == ContextPriority.REQUIRED
    assert packet.blocks[-1].text == "do thing X"
    assert "task_specification" in kinds


def test_generator_v1_does_not_emit_partial_plan_boundary(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="attempt spec framing",
        evaluation_criteria=["c1"],
        continuation_goal="future episode work",
    )
    task_store.upsert_task(
        task_id="t-1",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="do thing X",
        status="pending",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    packet = _generator_v1_build(
        ContextScope(
            mission_id=req.id,
            attempt_id=attempt.id,
            task_id="t-1",
        ),
        deps,
    )

    assert [b.kind for b in packet.blocks] == [
        "task_specification",
        "planned_task_spec",
    ]
    assert "future episode work" not in "\n".join(b.text for b in packet.blocks)


def test_generator_v1_dependency_summary_blocks(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    # Upstream task with a recorded summary.
    task_store.upsert_task(
        task_id="t-up",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="parent",
        status="done",
        summaries=[{"outcome": "success", "summary": "produced X"}],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )
    task_store.upsert_task(
        task_id="t-down",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="downstream",
        status="pending",
        summaries=[],
        needs=["t-up"],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    packet = _generator_v1_build(
        ContextScope(
            mission_id=req.id, attempt_id=attempt.id, task_id="t-down"
        ),
        deps,
    )
    dep_blocks = [b for b in packet.blocks if b.kind == "dependency_summary"]
    assert len(dep_blocks) == 1
    assert dep_blocks[0].metadata["dep_id"] == "t-up"
    assert dep_blocks[0].metadata["group_heading"] == "# Dependency Results"
    assert "produced X" in dep_blocks[0].text
    assert packet.blocks[-1].kind == "planned_task_spec"


def test_generator_v1_missing_dependency_task_raises_context_error(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    task_store.upsert_task(
        task_id="t-down",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="downstream",
        status="pending",
        summaries=[],
        needs=["t-missing"],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    with pytest.raises(ContextEngineError, match="Dependency task 't-missing'"):
        _generator_v1_build(
            ContextScope(
                mission_id=req.id,
                attempt_id=attempt.id,
                task_id="t-down",
            ),
            deps,
        )


# ---------------------------------------------------------------------------
# evaluator_v1
# ---------------------------------------------------------------------------


def test_evaluator_v1_emits_required_spec_and_criteria(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="evaluator spec",
        evaluation_criteria=["c1", "c2"],
        continuation_goal=None,
    )
    attempt_store.set_generator_task_ids(attempt.id, ["t-a"])
    task_store.upsert_task(
        task_id="t-a",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="x",
        status="done",
        summaries=[{"outcome": "success", "summary": "good output"}],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )
    packet = _evaluator_v1_build(
        ContextScope(
            mission_id=req.id, episode_id=episode.id, attempt_id=attempt.id
        ),
        deps,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == [
        "episode_goal",
        "task_specification",
        "completed_task_summary",
        "evaluation_criteria",
    ]
    assert all(
        b.priority == ContextPriority.REQUIRED
        for b in [packet.blocks[1], packet.blocks[-1]]
    )
    assert packet.blocks[0].metadata["heading"] == "# Mission / Current Episode"
    assert packet.blocks[2].metadata["group_heading"] == "# Dependency Results"


def test_evaluator_v1_renders_every_generator_summary_in_attempt_order(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="evaluator spec",
        evaluation_criteria=["all work passes"],
        continuation_goal=None,
    )
    task_ids = [f"t-{i}" for i in range(14)]
    attempt_store.set_generator_task_ids(attempt.id, task_ids)
    for task_id in task_ids:
        task_store.upsert_task(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            role="generator",
            agent_name="executor",
            rendered_prompt=f"work for {task_id}",
            status="done",
            summaries=[{"summary": f"summary for {task_id}"}],
            needs=[],
            task_center_attempt_id=attempt.id,
            spawn_reason="attempt_generator",
        )

    packet = _evaluator_v1_build(
        ContextScope(
            mission_id=req.id,
            episode_id=episode.id,
            attempt_id=attempt.id,
        ),
        deps,
    )

    summary_blocks = [
        b for b in packet.blocks if b.kind == "completed_task_summary"
    ]
    assert [b.source_id for b in summary_blocks] == task_ids
    assert [b.text for b in summary_blocks] == [
        f"summary for {task_id}" for task_id in task_ids
    ]
    assert all(block.priority == ContextPriority.HIGH for block in summary_blocks)
    assert all(
        block.metadata["group_heading"] == "# Dependency Results"
        for block in summary_blocks
    )
    assert packet.blocks[-1].kind == "evaluation_criteria"


def test_evaluator_v1_missing_generator_task_raises_context_error(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="evaluator spec",
        evaluation_criteria=["all work passes"],
        continuation_goal=None,
    )
    attempt_store.set_generator_task_ids(attempt.id, ["t-missing"])

    with pytest.raises(ContextEngineError, match="Generator task 't-missing'"):
        _evaluator_v1_build(
            ContextScope(
                mission_id=req.id,
                episode_id=episode.id,
                attempt_id=attempt.id,
            ),
            deps,
        )


def test_evaluator_v1_emits_partial_plan_boundary_before_summaries(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode = _seed_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="partial attempt spec",
        evaluation_criteria=["current slice passes"],
        continuation_goal="build admin tools next",
    )
    attempt_store.set_generator_task_ids(attempt.id, ["t-a"])
    task_store.upsert_task(
        task_id="t-a",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        rendered_prompt="x",
        status="done",
        summaries=[{"summary": "completed current slice"}],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    packet = _evaluator_v1_build(
        ContextScope(
            mission_id=req.id, episode_id=episode.id, attempt_id=attempt.id
        ),
        deps,
    )

    assert [b.kind for b in packet.blocks] == [
        "episode_goal",
        "task_specification",
        "partial_plan_boundary",
        "completed_task_summary",
        "evaluation_criteria",
    ]
    boundary = packet.blocks[2]
    assert boundary.priority == ContextPriority.REQUIRED
    assert "plan_kind: partial" in boundary.text
    assert "continuation_goal: build admin tools next" in boundary.text
    assert "Do not treat continuation work as missing" in boundary.text
    assert packet.blocks[-1].kind == "evaluation_criteria"


def test_evaluator_v1_episode2_frame_precedes_attempt_contract(
    deps, mission_store, episode_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_mission(mission_store, task_center_run_id)
    episode1 = _seed_episode(episode_store, mission_id=req.id)
    episode_store.close_succeeded(
        episode1.id,
        task_specification="accepted plan",
        task_summary="accepted summary",
        closed_at=datetime.now(UTC),
    )
    episode2 = _seed_continuation_episode(episode_store, mission_id=req.id)
    attempt = attempt_store.insert(episode_id=episode2.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        task_specification="attempt plan",
        evaluation_criteria=["criterion"],
        continuation_goal=None,
    )

    packet = _evaluator_v1_build(
        ContextScope(
            mission_id=req.id, episode_id=episode2.id, attempt_id=attempt.id
        ),
        deps,
    )

    assert [b.kind for b in packet.blocks] == [
        "mission_goal",
        "prior_episode_specification",
        "prior_episode_summary",
        "episode_goal",
        "task_specification",
        "evaluation_criteria",
    ]
    assert packet.blocks[0].metadata["heading"] == "# Mission"
    assert packet.blocks[1].metadata["group_heading"] == "# Previous Episode Results"
    assert packet.blocks[3].metadata["heading"] == "# Current Episode"
    assert packet.blocks[-1].kind == "evaluation_criteria"


# ---------------------------------------------------------------------------
# entry_executor_v1
# ---------------------------------------------------------------------------


def test_entry_executor_v1_emits_one_required_entry_request_block(
    deps, task_store, task_center_run_id
):
    task_store.upsert_task(
        task_id="entry",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="entry_executor",
        rendered_prompt="user prompt",
        status="running",
        summaries=[],
        needs=[],
        task_center_attempt_id=None,
        spawn_reason="entry_executor",
    )
    packet = _entry_executor_v1_build(
        ContextScope(task_id="entry"),
        deps,
    )
    assert packet.canonical_refs.mission_id is None
    assert packet.canonical_refs.task_id == "entry"
    assert len(packet.blocks) == 1
    block = packet.blocks[0]
    assert block.kind == "entry_request"
    assert block.priority == ContextPriority.REQUIRED
    assert block.text == "user prompt"
    # No mission_summary in entry-time context — it ships at close.
    assert all(b.kind != "mission_summary" for b in packet.blocks)


# ---------------------------------------------------------------------------
# register_builtin_recipes
# ---------------------------------------------------------------------------


def test_register_builtin_recipes_is_idempotent():
    from task_center.context_engine.recipes import register_builtin_recipes
    from task_center.context_engine.recipes_registry import RecipeRegistry

    saved = dict(RecipeRegistry._registry)
    RecipeRegistry.clear()
    try:
        register_builtin_recipes()
        first = set(RecipeRegistry.list_ids())
        register_builtin_recipes()
        second = set(RecipeRegistry.list_ids())
        assert first == second
        assert {
            "planner_v1",
            "generator_v1",
            "evaluator_v1",
            "entry_executor_v1",
        }.issubset(first)
    finally:
        RecipeRegistry.clear()
        RecipeRegistry._registry.update(saved)

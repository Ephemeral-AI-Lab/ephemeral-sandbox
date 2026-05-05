"""US-010: generator_v1, evaluator_v1, entry_executor_v1 happy-path."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.packet import ContextPriority
from task_center.context_engine.recipes.entry_executor import (
    _entry_executor_v1_build,
)
from task_center.context_engine.recipes.evaluator import _evaluator_v1_build
from task_center.context_engine.recipes.generator import _generator_v1_build
from task_center.context_engine.scope import ContextScope
from task_center.episode.episode import TaskSegmentCreationReason


@pytest.fixture
def deps(
    request_store, segment_store, graph_store, task_store
) -> ContextEngineDeps:
    return ContextEngineDeps(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
    )


def _seed_request(request_store, task_center_run_id):
    return request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal="overall",
    )


def _seed_segment(segment_store, *, request_id):
    return segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def _seed_continuation_segment(segment_store, *, request_id):
    return segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=2,
        creation_reason=TaskSegmentCreationReason.PARTIAL_CONTINUATION,
        goal="g2",
        attempt_budget=2,
    )


# ---------------------------------------------------------------------------
# generator_v1
# ---------------------------------------------------------------------------


def test_generator_v1_emits_planned_task_spec_required_block(
    deps, request_store, segment_store, graph_store, task_store, task_center_run_id
):
    req = _seed_request(request_store, task_center_run_id)
    seg = _seed_segment(segment_store, request_id=req.id)
    graph = graph_store.insert(task_segment_id=seg.id, graph_sequence_no=1)
    graph_store.set_plan_contract(
        graph.id,
        task_specification="graph spec framing",
        evaluation_criteria=["c1"],
        continuation_goal=None,
    )
    task_id = "t-1"
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="do thing X",
        status="pending",
        summaries=[],
        needs=[],
        task_center_harness_graph_id=graph.id,
        spawn_reason="harness_graph_generator",
    )
    packet = _generator_v1_build(
        ContextScope(
            request_id=req.id,
            harness_graph_id=graph.id,
            task_id=task_id,
        ),
        deps,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["task_specification", "planned_task_spec"]
    assert packet.blocks[-1].priority == ContextPriority.REQUIRED
    assert packet.blocks[-1].text == "do thing X"
    assert "task_specification" in kinds


def test_generator_v1_dependency_summary_blocks(
    deps, request_store, segment_store, graph_store, task_store, task_center_run_id
):
    req = _seed_request(request_store, task_center_run_id)
    seg = _seed_segment(segment_store, request_id=req.id)
    graph = graph_store.insert(task_segment_id=seg.id, graph_sequence_no=1)
    # Upstream task with a recorded summary.
    task_store.upsert_task(
        task_id="t-up",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="parent",
        status="done",
        summaries=[{"outcome": "success", "summary": "produced X"}],
        needs=[],
        task_center_harness_graph_id=graph.id,
        spawn_reason="harness_graph_generator",
    )
    task_store.upsert_task(
        task_id="t-down",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="downstream",
        status="pending",
        summaries=[],
        needs=["t-up"],
        task_center_harness_graph_id=graph.id,
        spawn_reason="harness_graph_generator",
    )

    packet = _generator_v1_build(
        ContextScope(
            request_id=req.id, harness_graph_id=graph.id, task_id="t-down"
        ),
        deps,
    )
    dep_blocks = [b for b in packet.blocks if b.kind == "dependency_summary"]
    assert len(dep_blocks) == 1
    assert dep_blocks[0].metadata["dep_id"] == "t-up"
    assert dep_blocks[0].metadata["group_heading"] == "# Dependency Results"
    assert "produced X" in dep_blocks[0].text
    assert packet.blocks[-1].kind == "planned_task_spec"


# ---------------------------------------------------------------------------
# evaluator_v1
# ---------------------------------------------------------------------------


def test_evaluator_v1_emits_required_spec_and_criteria(
    deps, request_store, segment_store, graph_store, task_store, task_center_run_id
):
    req = _seed_request(request_store, task_center_run_id)
    seg = _seed_segment(segment_store, request_id=req.id)
    graph = graph_store.insert(task_segment_id=seg.id, graph_sequence_no=1)
    graph_store.set_plan_contract(
        graph.id,
        task_specification="evaluator spec",
        evaluation_criteria=["c1", "c2"],
        continuation_goal=None,
    )
    graph_store.set_generator_task_ids(graph.id, ["t-a"])
    task_store.upsert_task(
        task_id="t-a",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="x",
        status="done",
        summaries=[{"outcome": "success", "summary": "good output"}],
        needs=[],
        task_center_harness_graph_id=graph.id,
        spawn_reason="harness_graph_generator",
    )
    packet = _evaluator_v1_build(
        ContextScope(
            request_id=req.id, segment_id=seg.id, harness_graph_id=graph.id
        ),
        deps,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == [
        "segment_goal",
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


def test_evaluator_v1_episode2_frame_precedes_attempt_contract(
    deps, request_store, segment_store, graph_store, task_store, task_center_run_id
):
    req = _seed_request(request_store, task_center_run_id)
    seg1 = _seed_segment(segment_store, request_id=req.id)
    segment_store.close_succeeded(
        seg1.id,
        task_specification="accepted plan",
        task_summary="accepted summary",
        closed_at=datetime.now(UTC),
    )
    seg2 = _seed_continuation_segment(segment_store, request_id=req.id)
    graph = graph_store.insert(task_segment_id=seg2.id, graph_sequence_no=1)
    graph_store.set_plan_contract(
        graph.id,
        task_specification="attempt plan",
        evaluation_criteria=["criterion"],
        continuation_goal=None,
    )

    packet = _evaluator_v1_build(
        ContextScope(
            request_id=req.id, segment_id=seg2.id, harness_graph_id=graph.id
        ),
        deps,
    )

    assert [b.kind for b in packet.blocks] == [
        "complex_task_goal",
        "prior_segment_specification",
        "prior_segment_summary",
        "segment_goal",
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
    deps, request_store, task_store, task_center_run_id
):
    req = _seed_request(request_store, task_center_run_id)
    task_store.upsert_task(
        task_id="entry",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="entry_executor",
        task_input="user prompt",
        status="running",
        summaries=[],
        needs=[],
        task_center_harness_graph_id=None,
        spawn_reason="entry_executor",
    )
    packet = _entry_executor_v1_build(
        ContextScope(request_id=req.id, task_id="entry"),
        deps,
    )
    assert len(packet.blocks) == 1
    block = packet.blocks[0]
    assert block.kind == "entry_request"
    assert block.priority == ContextPriority.REQUIRED
    assert block.text == "user prompt"
    # No complex_task_summary in entry-time context — it ships at close.
    assert all(b.kind != "complex_task_summary" for b in packet.blocks)


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

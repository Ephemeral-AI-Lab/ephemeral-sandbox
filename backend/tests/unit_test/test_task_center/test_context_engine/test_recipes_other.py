"""US-010: generator and evaluator recipe happy paths."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.context_engine.core import ContextEngineDeps, ContextEngineError
from task_center.context_engine.packet import ContextPriority
from task_center.context_engine.recipes.evaluator import build_evaluator_context
from task_center.context_engine.recipes.generator import build_generator_context
from task_center.context_engine.renderer import XmlPromptRenderer
from task_center.context_engine.scope import ContextScope
from task_center.iteration.state import IterationCreationReason


@pytest.fixture
def deps(
    goal_store, iteration_store, attempt_store, task_store
) -> ContextEngineDeps:
    return ContextEngineDeps(
        goal_store=goal_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )


def _seed_goal(goal_store, task_center_run_id):
    return goal_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="overall",
    )


def _seed_iteration(iteration_store, *, goal_id):
    return iteration_store.insert(
        goal_id=goal_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def _seed_continuation_iteration(iteration_store, *, goal_id):
    return iteration_store.insert(
        goal_id=goal_id,
        sequence_no=2,
        creation_reason=IterationCreationReason.DEFERRED_GOAL_CONTINUATION,
        goal="g2",
        attempt_budget=2,
    )


# ---------------------------------------------------------------------------
# generator — emits <plan_spec> (no wrapper), <dependency> siblings, <assigned_task>
# ---------------------------------------------------------------------------


def test_generator_emits_planned_task_spec_required_block(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="attempt spec framing",
        evaluation_criteria=["c1"],
        deferred_goal_for_next_iteration=None,
    )
    task_id = "t-1"
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="do thing X",
        status="pending",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )
    packet = build_generator_context(
        ContextScope(
            goal_id=req.id,
            attempt_id=attempt.id,
            task_id=task_id,
        ),
        deps,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["task_specification", "planned_task_spec"]
    plan_spec_block = packet.blocks[0]
    assert plan_spec_block.metadata["tag"] == "plan_spec"
    # No <attempt_plan> group wrapper.
    assert "group_tag" not in plan_spec_block.metadata
    assert packet.blocks[-1].kind == "planned_task_spec"
    assert packet.blocks[-1].priority == ContextPriority.REQUIRED
    assert packet.blocks[-1].text == "do thing X"
    assert packet.blocks[-1].metadata["tag"] == "assigned_task"


def test_generator_drops_deferred_goal_from_executor_packet(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """Continues-goal attempt emits only ``<plan_spec>`` to the executor — the
    ``<deferred_goal_for_next_iteration>`` is a planner / evaluator concern."""
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="attempt spec framing",
        evaluation_criteria=["c1"],
        deferred_goal_for_next_iteration="future iteration work",
    )
    task_store.upsert_task(
        task_id="t-1",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="do thing X",
        status="pending",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    packet = build_generator_context(
        ContextScope(
            goal_id=req.id,
            attempt_id=attempt.id,
            task_id="t-1",
        ),
        deps,
    )

    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["task_specification", "planned_task_spec"]
    plan_spec_block = packet.blocks[0]
    assert plan_spec_block.metadata["tag"] == "plan_spec"
    # No deferred-goal block survives in the executor packet.
    assert all(
        b.metadata.get("child_tag") != "deferred_goal_for_next_iteration"
        for b in packet.blocks
    )
    assert all(
        b.metadata.get("has_deferred_goal_for_next_iteration") != "true"
        for b in packet.blocks
    )


def test_generator_dependency_blocks_are_flat_siblings(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    task_store.upsert_task(
        task_id="t-up",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="parent",
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
        context_message="downstream",
        status="pending",
        summaries=[],
        needs=["t-up"],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    packet = build_generator_context(
        ContextScope(
            goal_id=req.id, attempt_id=attempt.id, task_id="t-down"
        ),
        deps,
    )
    dep_blocks = [b for b in packet.blocks if b.kind == "dependency_summary"]
    assert len(dep_blocks) == 1
    dep = dep_blocks[0]
    assert dep.metadata["tag"] == "dependency"
    # No <dependency_results> group wrapper.
    assert "group_tag" not in dep.metadata
    assert dep.metadata["attrs"] == 'id="t-up"'
    assert "produced X" in dep.text
    assert packet.blocks[-1].kind == "planned_task_spec"
    kinds = [b.kind for b in packet.blocks]
    dep_idx = kinds.index("dependency_summary")
    spec_idx = kinds.index("planned_task_spec")
    assert dep_idx < spec_idx


def test_generator_missing_dependency_task_raises_context_error(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    task_store.upsert_task(
        task_id="t-down",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="downstream",
        status="pending",
        summaries=[],
        needs=["t-missing"],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    with pytest.raises(ContextEngineError, match="Dependency task 't-missing'"):
        build_generator_context(
            ContextScope(
                goal_id=req.id,
                attempt_id=attempt.id,
                task_id="t-down",
            ),
            deps,
        )


# ---------------------------------------------------------------------------
# evaluator — flat current attempt: <plan_spec> + <task>×N + <evaluation_criteria>
# ---------------------------------------------------------------------------


def test_evaluator_emits_flat_plan_spec_tasks_and_criteria(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="evaluator spec",
        evaluation_criteria=["c1", "c2"],
        deferred_goal_for_next_iteration=None,
    )
    attempt_store.set_generator_task_ids(attempt.id, ["t-a"])
    task_store.upsert_task(
        task_id="t-a",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="x",
        status="done",
        summaries=[{"outcome": "success", "summary": "good output"}],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )
    packet = build_evaluator_context(
        ContextScope(
            goal_id=req.id, iteration_id=iteration.id, attempt_id=attempt.id
        ),
        deps,
    )
    # Flat top-level blocks — no goal/iteration frame, no <attempt> wrapper.
    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["task_specification", "generator_task_outcome", "evaluation_criteria"]
    plan_spec_block, task_block, criteria_block = packet.blocks
    # plan_spec — built fresh, top-level, no wrapper.
    assert plan_spec_block.metadata["tag"] == "plan_spec"
    assert "group_tag" not in plan_spec_block.metadata
    assert "pre_rendered_xml" not in plan_spec_block.metadata
    assert plan_spec_block.text == "evaluator spec"
    # task — summary-only body, id + status on the tag.
    assert task_block.metadata["tag"] == "task"
    assert task_block.metadata["attrs"] == 'id="t-a" status="done"'
    assert task_block.text == "good output"
    # evaluation_criteria — the authority, highest priority (last dropped).
    assert criteria_block.metadata["tag"] == "evaluation_criteria"
    assert criteria_block.priority == ContextPriority.REQUIRED
    assert criteria_block.text == "c1\nc2"
    # End-to-end: the flat blocks render through the real renderer. They are
    # ordinary (non pre_rendered_xml) blocks, so the renderer's structural-closer
    # guard sanitizes the bodies and wraps each tag once — no <attempt> wrapper.
    rendered = XmlPromptRenderer().render_context(packet)
    assert "<plan_spec>\nevaluator spec\n</plan_spec>" in rendered
    assert '<task id="t-a" status="done">\ngood output\n</task>' in rendered
    assert "<evaluation_criteria>\nc1\nc2\n</evaluation_criteria>" in rendered
    assert "<attempt" not in rendered and "<iteration" not in rendered


def test_evaluator_renders_every_generator_summary_in_order(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="evaluator spec",
        evaluation_criteria=["all work passes"],
        deferred_goal_for_next_iteration=None,
    )
    task_ids = [f"t-{i}" for i in range(14)]
    attempt_store.set_generator_task_ids(attempt.id, task_ids)
    for task_id in task_ids:
        task_store.upsert_task(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            role="generator",
            agent_name="executor",
            context_message=f"work for {task_id}",
            status="done",
            summaries=[{"summary": f"summary for {task_id}"}],
            needs=[],
            task_center_attempt_id=attempt.id,
            spawn_reason="attempt_generator",
        )

    packet = build_evaluator_context(
        ContextScope(
            goal_id=req.id,
            iteration_id=iteration.id,
            attempt_id=attempt.id,
        ),
        deps,
    )

    # One top-level <task> block per generator task, in submitted order.
    task_blocks = [b for b in packet.blocks if b.kind == "generator_task_outcome"]
    assert [b.source_id for b in task_blocks] == task_ids
    for task_id, block in zip(task_ids, task_blocks, strict=True):
        assert block.metadata["attrs"] == f'id="{task_id}" status="done"'
        assert block.text == f"summary for {task_id}"


def test_evaluator_missing_generator_task_surfaces_status(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """The recipe consults ``_generator_outcomes`` rather than rejecting; a
    missing task surfaces as a ``<task>`` block with ``status="missing task row"``
    and an empty body. The harness-level invariant violation surfaces via the
    planner submission accept path; the recipe is read-only."""
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="evaluator spec",
        evaluation_criteria=["all work passes"],
        deferred_goal_for_next_iteration=None,
    )
    attempt_store.set_generator_task_ids(attempt.id, ["t-missing"])

    packet = build_evaluator_context(
        ContextScope(
            goal_id=req.id,
            iteration_id=iteration.id,
            attempt_id=attempt.id,
        ),
        deps,
    )
    task_blocks = [b for b in packet.blocks if b.kind == "generator_task_outcome"]
    assert len(task_blocks) == 1
    assert task_blocks[0].metadata["attrs"] == 'id="t-missing" status="missing task row"'
    assert task_blocks[0].text == ""


def test_evaluator_defers_goal_attempt_has_no_deferred_block(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """Asymmetry guard: even when the attempt is a *defers-goal* plan, the
    evaluator packet carries NO ``<deferred_goal_for_next_iteration>`` block or
    metadata. The deferred remainder is the next iteration's contract, not
    evaluation evidence. (The planner still sees it in its failed-prior blocks —
    see test_attempts.py::test_prior_attempt_body_emits_deferred_goal_when_present.)
    """
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="partial attempt spec",
        evaluation_criteria=["current slice passes"],
        deferred_goal_for_next_iteration="build admin tools next",
    )
    attempt_store.set_generator_task_ids(attempt.id, ["t-a"])
    task_store.upsert_task(
        task_id="t-a",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="x",
        status="done",
        summaries=[{"summary": "completed current slice"}],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_generator",
    )

    packet = build_evaluator_context(
        ContextScope(
            goal_id=req.id, iteration_id=iteration.id, attempt_id=attempt.id
        ),
        deps,
    )

    # The plan_spec block is built fresh from attempt.plan_spec — no deferred child.
    plan_spec_block = next(b for b in packet.blocks if b.metadata.get("tag") == "plan_spec")
    assert plan_spec_block.text == "partial attempt spec"
    # No deferred-goal block, body, or signal anywhere in the packet.
    for block in packet.blocks:
        assert "deferred_goal_for_next_iteration" not in block.text
        assert "has_deferred_goal_for_next_iteration" not in block.metadata
        assert block.metadata.get("tag") != "deferred_goal_for_next_iteration"


def test_evaluator_iteration2_has_no_prior_iteration_frame(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """E4: even on iteration 2+ (with a closed prior iteration), the evaluator
    packet carries NO goal block, NO prior-iteration background, and NO
    <iteration> wrapper — only the flat current attempt."""
    req = _seed_goal(goal_store, task_center_run_id)
    iteration1 = _seed_iteration(iteration_store, goal_id=req.id)
    iteration_store.close_succeeded(
        iteration1.id,
        plan_spec="accepted plan",
        task_summary="accepted summary",
        closed_at=datetime.now(UTC),
    )
    iteration2 = _seed_continuation_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration2.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="attempt plan",
        evaluation_criteria=["criterion"],
        deferred_goal_for_next_iteration=None,
    )

    packet = build_evaluator_context(
        ContextScope(
            goal_id=req.id, iteration_id=iteration2.id, attempt_id=attempt.id
        ),
        deps,
    )

    # No goal/prior-iteration/iteration-frame kinds survive.
    assert [b.kind for b in packet.blocks] == ["task_specification", "evaluation_criteria"]
    assert all("group_tag" not in b.metadata for b in packet.blocks)
    tags = {b.metadata.get("tag") for b in packet.blocks}
    assert tags == {"plan_spec", "evaluation_criteria"}
    # iteration is still threaded into canonical refs via attempt.iteration_id.
    assert packet.canonical_refs.iteration_id == iteration2.id


def test_evaluator_with_empty_criteria_omits_criteria_block(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """When evaluation_criteria is empty, no <evaluation_criteria> block is
    emitted; the packet still carries the <plan_spec> framing."""
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="evaluator spec",
        evaluation_criteria=[],
        deferred_goal_for_next_iteration=None,
    )

    packet = build_evaluator_context(
        ContextScope(
            goal_id=req.id, iteration_id=iteration.id, attempt_id=attempt.id
        ),
        deps,
    )

    assert "evaluation_criteria" not in [b.kind for b in packet.blocks]
    assert [b.metadata.get("tag") for b in packet.blocks] == ["plan_spec"]


def test_evaluator_builds_without_goal_or_iteration_scope(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """U1-AC5: the recipe requires only attempt_id; with goal_id/iteration_id
    absent from the scope the packet still builds, deriving iteration from
    attempt.iteration_id."""
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    attempt_store.set_plan_contract(
        attempt.id,
        plan_spec="evaluator spec",
        evaluation_criteria=["c1"],
        deferred_goal_for_next_iteration=None,
    )

    packet = build_evaluator_context(ContextScope(attempt_id=attempt.id), deps)

    assert [b.metadata.get("tag") for b in packet.blocks] == ["plan_spec", "evaluation_criteria"]
    assert packet.canonical_refs.iteration_id == iteration.id
    assert packet.canonical_refs.goal_id is None


def test_evaluator_omitted_blocks_when_no_plan_spec(
    deps, goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    """Degenerate guard: an attempt with no submitted plan yields an empty
    packet (the evaluator only runs post-plan-submission in practice)."""
    req = _seed_goal(goal_store, task_center_run_id)
    iteration = _seed_iteration(iteration_store, goal_id=req.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)

    packet = build_evaluator_context(ContextScope(attempt_id=attempt.id), deps)

    assert packet.blocks == []


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
            "planner",
            "generator",
            "evaluator",
        }.issubset(first)
    finally:
        RecipeRegistry.clear()
        RecipeRegistry._registry.update(saved)

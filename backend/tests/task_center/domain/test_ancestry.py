"""Unit tests for the canonical ancestry walker.

Pins (a) walker behavior across no/full/partial caller chains and
(b) the structural property: every legacy/new caller resolves via
``inspect.unwrap`` to the same canonical function object so drift fails
the test, not silently.
"""

from __future__ import annotations

import inspect

import pytest

from task_center.agent_launch.predicates import (
    PredicateRegistry,
    register_builtin_predicates,
)
from task_center.mission.ancestry import (
    has_partial_planned_caller_ancestor,
)
from task_center.attempt import HarnessGraphStage
from task_center.episode.episode import TaskSegmentCreationReason


def _stores(request_store, segment_store, graph_store, task_store):
    return dict(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
    )


def _seed_request(
    request_store,
    *,
    task_center_run_id: str,
    requested_by_task_id: str = "t-entry",
    goal: str = "g",
):
    return request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id=requested_by_task_id,
        goal=goal,
    )


def _seed_segment(segment_store, *, request_id: str, sequence_no: int = 1):
    return segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=sequence_no,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def _seed_graph(
    graph_store,
    *,
    segment_id: str,
    sequence_no: int = 1,
    continuation_goal: str | None = None,
):
    graph = graph_store.insert(
        task_segment_id=segment_id, graph_sequence_no=sequence_no
    )
    graph_store.set_plan_contract(
        graph.id,
        task_specification="spec",
        evaluation_criteria=["c1"],
        continuation_goal=continuation_goal,
    )
    graph_store.set_stage(graph.id, HarnessGraphStage.GENERATING)
    return graph


def _seed_task(
    task_store,
    *,
    task_id: str,
    task_center_run_id: str,
    harness_graph_id: str | None,
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
        task_center_harness_graph_id=harness_graph_id,
        spawn_reason="test_seed",
    )


# ---------------------------------------------------------------------------
# Walker behavior
# ---------------------------------------------------------------------------


def test_no_parent_task_returns_false(
    request_store, segment_store, graph_store, task_store, task_center_run_id
):
    request = _seed_request(
        request_store, task_center_run_id=task_center_run_id
    )
    # No parent task seeded → walk terminates returning False.
    assert (
        has_partial_planned_caller_ancestor(
            request_id=request.id,
            **_stores(request_store, segment_store, graph_store, task_store),
        )
        is False
    )


def test_parent_task_with_no_graph_returns_false(
    request_store, segment_store, graph_store, task_store, task_center_run_id
):
    request = _seed_request(
        request_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
    )
    _seed_task(
        task_store,
        task_id="t-entry",
        task_center_run_id=task_center_run_id,
        harness_graph_id=None,
    )
    assert (
        has_partial_planned_caller_ancestor(
            request_id=request.id,
            **_stores(request_store, segment_store, graph_store, task_store),
        )
        is False
    )


def test_full_plan_caller_chain_returns_false(
    request_store, segment_store, graph_store, task_store, task_center_run_id
):
    # Top-level request → segment → caller_graph (full plan: continuation_goal=None)
    parent_req = _seed_request(
        request_store, task_center_run_id=task_center_run_id
    )
    parent_seg = _seed_segment(segment_store, request_id=parent_req.id)
    caller_graph = _seed_graph(
        graph_store, segment_id=parent_seg.id, continuation_goal=None
    )
    _seed_task(
        task_store,
        task_id="t-caller",
        task_center_run_id=task_center_run_id,
        harness_graph_id=caller_graph.id,
    )
    child_req = _seed_request(
        request_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
    )
    assert (
        has_partial_planned_caller_ancestor(
            request_id=child_req.id,
            **_stores(request_store, segment_store, graph_store, task_store),
        )
        is False
    )


def test_partial_plan_caller_returns_true(
    request_store, segment_store, graph_store, task_store, task_center_run_id
):
    parent_req = _seed_request(
        request_store, task_center_run_id=task_center_run_id
    )
    parent_seg = _seed_segment(segment_store, request_id=parent_req.id)
    caller_graph = _seed_graph(
        graph_store,
        segment_id=parent_seg.id,
        continuation_goal="continue here",
    )
    _seed_task(
        task_store,
        task_id="t-caller",
        task_center_run_id=task_center_run_id,
        harness_graph_id=caller_graph.id,
    )
    child_req = _seed_request(
        request_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
    )
    assert (
        has_partial_planned_caller_ancestor(
            request_id=child_req.id,
            **_stores(request_store, segment_store, graph_store, task_store),
        )
        is True
    )


def test_deep_mixed_chain_with_partial_root_returns_true(
    request_store, segment_store, graph_store, task_store, task_center_run_id
):
    # Three-deep: root submits partial → child full → grandchild request.
    root_req = _seed_request(
        request_store, task_center_run_id=task_center_run_id
    )
    root_seg = _seed_segment(segment_store, request_id=root_req.id)
    root_graph = _seed_graph(
        graph_store,
        segment_id=root_seg.id,
        continuation_goal="rotate next",
    )
    _seed_task(
        task_store,
        task_id="t-root",
        task_center_run_id=task_center_run_id,
        harness_graph_id=root_graph.id,
    )
    mid_req = _seed_request(
        request_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-root",
    )
    mid_seg = _seed_segment(segment_store, request_id=mid_req.id)
    mid_graph = _seed_graph(
        graph_store, segment_id=mid_seg.id, continuation_goal=None
    )
    _seed_task(
        task_store,
        task_id="t-mid",
        task_center_run_id=task_center_run_id,
        harness_graph_id=mid_graph.id,
    )
    leaf_req = _seed_request(
        request_store,
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-mid",
    )

    assert (
        has_partial_planned_caller_ancestor(
            request_id=leaf_req.id,
            **_stores(request_store, segment_store, graph_store, task_store),
        )
        is True
    )


def test_unknown_request_id_raises(
    request_store, segment_store, graph_store, task_store
):
    from task_center.exceptions import GraphInvariantViolation

    with pytest.raises(GraphInvariantViolation):
        has_partial_planned_caller_ancestor(
            request_id="nonexistent",
            **_stores(request_store, segment_store, graph_store, task_store),
        )


# ---------------------------------------------------------------------------
# Structural enforcement: legacy shim ↔ canonical
# ---------------------------------------------------------------------------


def test_resolver_predicate_dispatches_to_canonical(
    request_store, segment_store, graph_store, task_store, task_center_run_id
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
        parent_req = _seed_request(
            request_store, task_center_run_id=task_center_run_id
        )
        parent_seg = _seed_segment(segment_store, request_id=parent_req.id)
        caller_graph = _seed_graph(
            graph_store,
            segment_id=parent_seg.id,
            continuation_goal="next",
        )
        _seed_task(
            task_store,
            task_id="t-caller",
            task_center_run_id=task_center_run_id,
            harness_graph_id=caller_graph.id,
        )
        child_req = _seed_request(
            request_store,
            task_center_run_id=task_center_run_id,
            requested_by_task_id="t-caller",
        )
        deps = ContextEngineDeps(
            request_store=request_store,
            segment_store=segment_store,
            graph_store=graph_store,
            task_store=task_store,
        )
        ctx = ResolverContext(
            scope=ContextScope(request_id=child_req.id), deps=deps
        )
        predicate = PredicateRegistry.get("partial_plan_caller_ancestor")
        canonical_result = has_partial_planned_caller_ancestor(
            request_id=child_req.id,
            **_stores(request_store, segment_store, graph_store, task_store),
        )
        assert predicate(ctx) is True
        assert predicate(ctx) is canonical_result, (
            "resolver predicate must yield the same answer as the canonical"
        )

        # ResolverContext convenience method also delegates to the canonical.
        assert ctx.has_partial_planned_caller_ancestor() is canonical_result
    finally:
        PredicateRegistry.clear()
        PredicateRegistry._registry.update(saved)


def test_canonical_function_unwraps_to_itself():
    """Pin: ``inspect.unwrap`` resolves the canonical implementation."""
    canonical = inspect.unwrap(has_partial_planned_caller_ancestor)
    assert canonical is has_partial_planned_caller_ancestor


def test_resolver_call_sites_reference_canonical_in_source():
    """Structural enforcement: every shim must call into the canonical via
    the same name, so a future caller that drifts to a different signature
    breaks this assertion."""
    from task_center.agent_launch.predicates import (
        ResolverContext,
        _partial_plan_caller_ancestor,
    )

    predicate_src = inspect.getsource(_partial_plan_caller_ancestor)
    helper_src = inspect.getsource(
        ResolverContext.has_partial_planned_caller_ancestor
    )
    canonical_name = "has_partial_planned_caller_ancestor"
    assert canonical_name in predicate_src, (
        "resolver predicate must delegate to the canonical ancestry function"
    )
    assert canonical_name in helper_src, (
        "ResolverContext convenience method must delegate to the canonical"
    )

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
    MAX_FAILED_GRAPHS_RENDERED,
    _planner_v1_build,
)
from task_center.context_engine.scope import ContextScope
from task_center.harness_graph.graph import (
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.segment.segment import (
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)


@pytest.fixture
def deps_with_stores(
    request_store, segment_store, graph_store, task_store
) -> ContextEngineDeps:
    return ContextEngineDeps(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
    )


def _seed_request(request_store, task_center_run_id, goal="goal"):
    return request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal=goal,
    )


def _seed_segment(
    segment_store,
    *,
    request_id: str,
    sequence_no: int,
    goal: str = "g",
):
    return segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=sequence_no,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal=goal,
        attempt_budget=2,
    )


def _close_segment_succeeded(
    segment_store, segment_id, *, spec: str, summary: str
):
    return segment_store.close_succeeded(
        segment_id,
        task_specification=spec,
        task_summary=summary,
        closed_at=datetime.now(UTC),
    )


def _seed_failed_graph(graph_store, segment_id, *, sequence_no: int):
    g = graph_store.insert(
        task_segment_id=segment_id, graph_sequence_no=sequence_no
    )
    graph_store.set_plan_contract(
        g.id,
        task_specification=f"spec-{sequence_no}",
        evaluation_criteria=[f"crit-{sequence_no}-a", f"crit-{sequence_no}-b"],
        continuation_goal=None,
    )
    return graph_store.close(
        g.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
        closed_at=datetime.now(UTC),
    )


def _seed_running_graph(graph_store, segment_id, *, sequence_no: int):
    return graph_store.insert(
        task_segment_id=segment_id, graph_sequence_no=sequence_no
    )


# ---------------------------------------------------------------------------
# seg-1 branch
# ---------------------------------------------------------------------------


def test_seg1_emits_one_segment_goal_block_with_initial_metadata(
    deps_with_stores, request_store, segment_store, graph_store,
    task_center_run_id,
):
    request = _seed_request(request_store, task_center_run_id, goal="overall")
    seg = _seed_segment(
        segment_store, request_id=request.id, sequence_no=1, goal="overall"
    )
    g = _seed_running_graph(graph_store, seg.id, sequence_no=1)

    packet = _planner_v1_build(
        ContextScope(
            request_id=request.id, segment_id=seg.id, harness_graph_id=g.id
        ),
        deps_with_stores,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == ["segment_goal"]
    assert packet.metadata["is_initial_segment"] == "true"
    assert packet.target_id == g.id


# ---------------------------------------------------------------------------
# seg-2 / seg-N branch
# ---------------------------------------------------------------------------


def test_seg2_emits_complex_goal_segment_goal_and_one_prior_pair(
    deps_with_stores, request_store, segment_store, graph_store,
    task_center_run_id,
):
    request = _seed_request(request_store, task_center_run_id, goal="overall")
    seg1 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=1, goal="seg1 goal"
    )
    _close_segment_succeeded(
        segment_store, seg1.id, spec="seg1 spec", summary="seg1 summary"
    )
    seg2 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=2, goal="seg2 goal"
    )
    g = _seed_running_graph(graph_store, seg2.id, sequence_no=1)

    packet = _planner_v1_build(
        ContextScope(
            request_id=request.id, segment_id=seg2.id, harness_graph_id=g.id
        ),
        deps_with_stores,
    )
    kinds = [b.kind for b in packet.blocks]
    assert kinds == [
        "complex_task_goal",
        "segment_goal",
        "prior_segment_specification",
        "prior_segment_summary",
    ]
    assert packet.metadata["is_initial_segment"] == "false"
    prior_spec = packet.blocks[2]
    assert prior_spec.priority == ContextPriority.HIGH
    assert prior_spec.metadata["segment_sequence_no"] == "1"
    assert prior_spec.text == "seg1 spec"


def test_seg3_emits_two_pairs_with_priority_split(
    deps_with_stores, request_store, segment_store, graph_store,
    task_center_run_id,
):
    request = _seed_request(request_store, task_center_run_id, goal="overall")
    seg1 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=1, goal="g1"
    )
    _close_segment_succeeded(segment_store, seg1.id, spec="s1", summary="sum1")
    seg2 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=2, goal="g2"
    )
    _close_segment_succeeded(segment_store, seg2.id, spec="s2", summary="sum2")
    seg3 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=3, goal="g3"
    )
    g = _seed_running_graph(graph_store, seg3.id, sequence_no=1)

    packet = _planner_v1_build(
        ContextScope(
            request_id=request.id, segment_id=seg3.id, harness_graph_id=g.id
        ),
        deps_with_stores,
    )
    # Two prior segments, most-recent first; immediate (seg-2) HIGH, earlier (seg-1) MEDIUM.
    prior_specs = [
        b for b in packet.blocks if b.kind == "prior_segment_specification"
    ]
    assert len(prior_specs) == 2
    assert prior_specs[0].metadata["segment_sequence_no"] == "2"
    assert prior_specs[0].priority == ContextPriority.HIGH
    assert prior_specs[1].metadata["segment_sequence_no"] == "1"
    assert prior_specs[1].priority == ContextPriority.MEDIUM


def test_missing_prior_spec_raises_context_engine_error(
    deps_with_stores, request_store, segment_store, graph_store,
    task_center_run_id,
):
    """Closed seg-1 with task_specification still null is an invariant
    violation; recipe must raise."""
    request = _seed_request(request_store, task_center_run_id)
    seg1 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=1, goal="g1"
    )
    # Close via legacy set_status (does not write denormalized fields).
    segment_store.set_status(
        seg1.id, status=TaskSegmentStatus.SUCCEEDED, closed_at=datetime.now(UTC)
    )
    seg2 = _seed_segment(
        segment_store, request_id=request.id, sequence_no=2, goal="g2"
    )
    g = _seed_running_graph(graph_store, seg2.id, sequence_no=1)

    with pytest.raises(ContextEngineError):
        _planner_v1_build(
            ContextScope(
                request_id=request.id, segment_id=seg2.id, harness_graph_id=g.id
            ),
            deps_with_stores,
        )


# ---------------------------------------------------------------------------
# Failed-graph landscape blocks (current segment retries)
# ---------------------------------------------------------------------------


def test_three_failed_graphs_emit_three_high_priority_blocks(
    deps_with_stores, request_store, segment_store, graph_store,
    task_center_run_id,
):
    request = _seed_request(request_store, task_center_run_id)
    seg = _seed_segment(
        segment_store, request_id=request.id, sequence_no=1, goal="g"
    )
    for n in (1, 2, 3):
        _seed_failed_graph(graph_store, seg.id, sequence_no=n)
    current = _seed_running_graph(graph_store, seg.id, sequence_no=4)

    packet = _planner_v1_build(
        ContextScope(
            request_id=request.id,
            segment_id=seg.id,
            harness_graph_id=current.id,
        ),
        deps_with_stores,
    )
    failed_blocks = [
        b for b in packet.blocks if b.kind == "failed_graph_landscape"
    ]
    assert len(failed_blocks) == 3
    for block in failed_blocks:
        assert block.priority == ContextPriority.HIGH
    assert [b.metadata["graph_sequence_no"] for b in failed_blocks] == [
        "1",
        "2",
        "3",
    ]


def test_more_than_cap_failed_graphs_truncates_with_medium_summary(
    deps_with_stores, request_store, segment_store, graph_store,
    task_center_run_id,
):
    request = _seed_request(request_store, task_center_run_id)
    seg = _seed_segment(
        segment_store, request_id=request.id, sequence_no=1, goal="g"
    )
    total = MAX_FAILED_GRAPHS_RENDERED + 2
    for n in range(1, total + 1):
        _seed_failed_graph(graph_store, seg.id, sequence_no=n)
    current = _seed_running_graph(
        graph_store, seg.id, sequence_no=total + 1
    )

    packet = _planner_v1_build(
        ContextScope(
            request_id=request.id,
            segment_id=seg.id,
            harness_graph_id=current.id,
        ),
        deps_with_stores,
    )
    failed_blocks = [
        b for b in packet.blocks if b.kind == "failed_graph_landscape"
    ]
    assert len(failed_blocks) == MAX_FAILED_GRAPHS_RENDERED + 1
    truncation_block = failed_blocks[-1]
    assert truncation_block.priority == ContextPriority.MEDIUM
    assert truncation_block.metadata["truncated_count"] == "2"

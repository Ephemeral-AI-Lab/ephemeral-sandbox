"""``planner_v1`` recipe — context for one HarnessGraph planner spawn.

See plan §3.3.6 for the full block taxonomy. The recipe reads:

* the current segment row (``segment_goal``);
* the request row (``complex_task_goal`` for seg>1);
* every prior closed-succeeded segment row (``prior_segment_*`` blocks,
  paired and labelled with ``segment_sequence_no``);
* every failed graph in the current segment except the running one
  (``failed_graph_landscape`` blocks, ordered by ``graph_sequence_no``).

The recipe is a pure builder: it reads stores and returns a
:class:`ContextPacket`. No renderer calls, no lifecycle mutations.
"""

from __future__ import annotations

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.scope import ContextScope
from task_center.harness_graph.graph import HarnessGraph, HarnessGraphStatus
from task_center.segment.segment import TaskSegment, TaskSegmentStatus

PLANNER_V1 = "planner_v1"
MAX_FAILED_GRAPHS_RENDERED = 6
_REQUIRED_FIELDS = frozenset(
    {"request_id", "segment_id", "harness_graph_id"}
)


def _planner_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    request = deps.request_store.get(scope.request_id)
    if request is None:
        raise ContextEngineError(
            f"ComplexTaskRequest {scope.request_id!r} not found"
        )
    segment = deps.segment_store.get(scope.segment_id)
    if segment is None:
        raise ContextEngineError(
            f"TaskSegment {scope.segment_id!r} not found"
        )

    metadata: dict[str, str] = {}
    blocks: list[ContextBlock] = []

    if segment.sequence_no == 1:
        metadata["is_initial_segment"] = "true"
        blocks.append(_segment_goal_block(segment))
    else:
        metadata["is_initial_segment"] = "false"
        blocks.append(_complex_task_goal_block(request))
        blocks.append(_segment_goal_block(segment))
        blocks.extend(
            _prior_segment_blocks(
                segment,
                segments=deps.segment_store.list_for_request(request.id),
            )
        )

    blocks.extend(
        _failed_graph_landscape_blocks(
            current_graph_id=scope.harness_graph_id,
            graphs=deps.graph_store.list_for_segment(segment.id),
        )
    )

    return ContextPacket(
        target_role="planner",
        target_id=scope.harness_graph_id,
        canonical_refs=ContextRefs(
            request_id=request.id,
            segment_id=segment.id,
            harness_graph_id=scope.harness_graph_id,
        ),
        blocks=blocks,
        metadata=metadata,
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


# ---------------------------------------------------------------------------
# Block builders — each takes plain DTOs so unit tests can drive them
# directly without round-tripping the engine.
# ---------------------------------------------------------------------------


def _segment_goal_block(segment: TaskSegment) -> ContextBlock:
    return ContextBlock(
        kind=ContextBlockKind.SEGMENT_GOAL,
        priority=ContextPriority.REQUIRED,
        text=segment.goal,
        source_id=segment.id,
        source_kind="task_segment",
    )


def _complex_task_goal_block(request) -> ContextBlock:  # type: ignore[no-untyped-def]
    return ContextBlock(
        kind=ContextBlockKind.COMPLEX_TASK_GOAL,
        priority=ContextPriority.REQUIRED,
        text=request.goal,
        source_id=request.id,
        source_kind="complex_task_request",
    )


def _prior_segment_blocks(
    current: TaskSegment, *, segments: list[TaskSegment]
) -> list[ContextBlock]:
    priors = sorted(
        (s for s in segments if s.sequence_no < current.sequence_no),
        key=lambda s: s.sequence_no,
        reverse=True,
    )
    out: list[ContextBlock] = []
    for idx, prior in enumerate(priors):
        if prior.task_specification is None or prior.task_summary is None:
            raise ContextEngineError(
                f"Prior segment {prior.id!r} (seq={prior.sequence_no}) is "
                "missing task_specification or task_summary; chain "
                "integrity violated."
            )
        priority = ContextPriority.HIGH if idx == 0 else ContextPriority.MEDIUM
        block_meta = {"segment_sequence_no": str(prior.sequence_no)}
        out.append(
            ContextBlock(
                kind=ContextBlockKind.PRIOR_SEGMENT_SPECIFICATION,
                priority=priority,
                text=prior.task_specification,
                source_id=prior.id,
                source_kind="task_segment",
                metadata=block_meta,
            )
        )
        out.append(
            ContextBlock(
                kind=ContextBlockKind.PRIOR_SEGMENT_SUMMARY,
                priority=priority,
                text=prior.task_summary,
                source_id=prior.id,
                source_kind="task_segment",
                metadata=block_meta,
            )
        )
    return out


def _failed_graph_landscape_blocks(
    *,
    current_graph_id: str | None,
    graphs: list[HarnessGraph],
) -> list[ContextBlock]:
    failed = sorted(
        (
            g
            for g in graphs
            if g.status == HarnessGraphStatus.FAILED
            and g.id != current_graph_id
        ),
        key=lambda g: g.graph_sequence_no,
    )
    if not failed:
        return []

    if len(failed) <= MAX_FAILED_GRAPHS_RENDERED:
        rendered = failed
        truncated: list[HarnessGraph] = []
    else:
        rendered = failed[-MAX_FAILED_GRAPHS_RENDERED:]
        truncated = failed[:-MAX_FAILED_GRAPHS_RENDERED]

    blocks: list[ContextBlock] = [
        ContextBlock(
            kind=ContextBlockKind.FAILED_GRAPH_LANDSCAPE,
            priority=ContextPriority.HIGH,
            text=_render_failed_graph(g),
            source_id=g.id,
            source_kind="harness_graph",
            metadata={"graph_sequence_no": str(g.graph_sequence_no)},
        )
        for g in rendered
    ]

    if truncated:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.FAILED_GRAPH_LANDSCAPE,
                priority=ContextPriority.MEDIUM,
                text=(
                    f"{len(truncated)} earlier failed attempts omitted "
                    f"(graph_sequence_no "
                    f"{truncated[0].graph_sequence_no}–"
                    f"{truncated[-1].graph_sequence_no}). "
                    f"Most recent {MAX_FAILED_GRAPHS_RENDERED} attempts "
                    f"shown above."
                ),
                source_id=None,
                source_kind=None,
                metadata={"truncated_count": str(len(truncated))},
            )
        )
    return blocks


def _render_failed_graph(graph: HarnessGraph) -> str:
    criteria_block = (
        "\n".join(f"  - {c}" for c in graph.evaluation_criteria) or "  (none)"
    )
    return (
        f"task_specification: {graph.task_specification or '(missing)'}\n"
        f"evaluation_criteria:\n{criteria_block}\n"
        f"fail_reason: {graph.fail_reason.value if graph.fail_reason else 'unknown'}"
    )


PLANNER_V1_RECIPE = ContextRecipe(
    id=PLANNER_V1,
    required_scope_fields=_REQUIRED_FIELDS,
    build=_planner_v1_build,
)


# Light re-export so tests / other recipes can reuse the segment status enum.
__all__ = [
    "PLANNER_V1",
    "PLANNER_V1_RECIPE",
    "MAX_FAILED_GRAPHS_RENDERED",
    "TaskSegmentStatus",
]

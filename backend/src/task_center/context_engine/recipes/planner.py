"""``planner_v1`` recipe — context for one attempt planner spawn.

See plan §3.3.6 for the full block taxonomy. The recipe reads:

* the mission / current episode frame;
* every prior closed-succeeded episode projection for episode 2+;
* every failed attempt in the current episode except the running one
  (``failed_attempt_landscape`` blocks, ordered by ``graph_sequence_no``).

The recipe is a pure builder: it reads stores and returns a
:class:`ContextPacket`. No renderer calls, no lifecycle mutations.
"""

from __future__ import annotations

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import (
    ContextPacket,
    ContextRefs,
)
from task_center.context_engine.recipes._mission_episode import (
    mission_episode_blocks,
)
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.recipes.attempt_landscape import (
    MAX_FAILED_ATTEMPTS_RENDERED,
    failed_attempt_landscape_blocks,
)
from task_center.context_engine.scope import ContextScope

PLANNER_V1 = "planner_v1"
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

    blocks = mission_episode_blocks(
        request=request,
        current_segment=segment,
        segments=deps.segment_store.list_for_request(request.id),
    )

    blocks.extend(
        failed_attempt_landscape_blocks(
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
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


PLANNER_V1_RECIPE = ContextRecipe(
    id=PLANNER_V1,
    required_scope_fields=_REQUIRED_FIELDS,
    build=_planner_v1_build,
)


__all__ = [
    "PLANNER_V1",
    "PLANNER_V1_RECIPE",
    "MAX_FAILED_ATTEMPTS_RENDERED",
]

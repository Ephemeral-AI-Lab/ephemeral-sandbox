"""``evaluator_v1`` recipe — context for one evaluator spawn.

Emits mission/episode framing, the current attempt plan, dependency results,
and the evaluation criteria in presentation order. The criteria block remains
last so pass/fail authority is anchored to the current attempt contract.
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
from task_center.context_engine.recipes._summaries import latest_summary_text
from task_center.context_engine.recipes._mission_episode import (
    mission_episode_blocks,
)
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.scope import ContextScope

EVALUATOR_V1 = "evaluator_v1"
_REQUIRED_FIELDS = frozenset({"request_id", "harness_graph_id"})


def _evaluator_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    graph = deps.graph_store.get(scope.harness_graph_id)
    if graph is None:
        raise ContextEngineError(
            f"HarnessGraph {scope.harness_graph_id!r} not found"
        )
    request = deps.request_store.get(scope.request_id)
    if request is None:
        raise ContextEngineError(
            f"ComplexTaskRequest {scope.request_id!r} not found"
        )
    segment_id = scope.segment_id or graph.task_segment_id
    segment = deps.segment_store.get(segment_id)
    if segment is None:
        raise ContextEngineError(f"TaskSegment {segment_id!r} not found")

    blocks = mission_episode_blocks(
        request=request,
        current_segment=segment,
        segments=deps.segment_store.list_for_request(request.id),
    )
    if graph.task_specification:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.TASK_SPECIFICATION,
                priority=ContextPriority.REQUIRED,
                text=graph.task_specification,
                source_id=graph.id,
                source_kind="harness_graph",
            )
        )

    for task_id in graph.generator_task_ids:
        task = deps.task_store.get_task(task_id)
        if task is None:
            continue
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.COMPLETED_TASK_SUMMARY,
                priority=ContextPriority.HIGH,
                text=latest_summary_text(task.get("summaries")),
                source_id=task_id,
                source_kind="task_center_task",
                metadata={
                    "task_id": task_id,
                    "group_heading": "# Dependency Results",
                    "subheading": str(task.get("id") or task_id),
                },
            )
        )
    criteria = list(graph.evaluation_criteria)
    if criteria:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.EVALUATION_CRITERIA,
                priority=ContextPriority.REQUIRED,
                text="\n".join(f"- {c}" for c in criteria),
                source_id=graph.id,
                source_kind="harness_graph",
            )
        )

    return ContextPacket(
        target_role="evaluator",
        target_id=scope.harness_graph_id,
        canonical_refs=ContextRefs(
            request_id=scope.request_id,
            segment_id=segment.id,
            harness_graph_id=scope.harness_graph_id,
        ),
        blocks=blocks,
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


EVALUATOR_V1_RECIPE = ContextRecipe(
    id=EVALUATOR_V1,
    required_scope_fields=_REQUIRED_FIELDS,
    build=_evaluator_v1_build,
)


__all__ = ["EVALUATOR_V1", "EVALUATOR_V1_RECIPE"]

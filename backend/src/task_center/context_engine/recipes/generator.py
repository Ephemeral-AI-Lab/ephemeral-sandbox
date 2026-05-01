"""``generator_v1`` recipe — context for one generator task spawn.

Emits the planner-supplied ``planned_task_spec`` block (priority=required,
sourced from the task row's ``task_input``), the graph's
``task_specification`` framing, and one ``dependency_summary`` per upstream
task.
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

GENERATOR_V1 = "generator_v1"
_REQUIRED_FIELDS = frozenset({"request_id", "harness_graph_id", "task_id"})


def _generator_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    graph = deps.graph_store.get(scope.harness_graph_id)
    if graph is None:
        raise ContextEngineError(
            f"HarnessGraph {scope.harness_graph_id!r} not found"
        )
    task = deps.task_store.get_task(scope.task_id)
    if task is None:
        raise ContextEngineError(
            f"TaskCenterTask {scope.task_id!r} not found"
        )

    blocks: list[ContextBlock] = [
        ContextBlock(
            kind=ContextBlockKind.PLANNED_TASK_SPEC,
            priority=ContextPriority.REQUIRED,
            text=str(task.get("task_input") or ""),
            source_id=scope.task_id,
            source_kind="task_center_task",
        )
    ]
    if graph.task_specification:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.TASK_SPECIFICATION,
                priority=ContextPriority.HIGH,
                text=graph.task_specification,
                source_id=graph.id,
                source_kind="harness_graph",
            )
        )

    blocks.extend(
        _dependency_summary_blocks(
            needs=task.get("needs") or (),
            harness_graph_id=scope.harness_graph_id,
            task_store=deps.task_store,
        )
    )

    return ContextPacket(
        target_role="generator",
        target_id=scope.task_id,
        canonical_refs=ContextRefs(
            request_id=scope.request_id,
            segment_id=scope.segment_id,
            harness_graph_id=scope.harness_graph_id,
            task_id=scope.task_id,
        ),
        blocks=blocks,
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


def _dependency_summary_blocks(
    *,
    needs,  # type: ignore[no-untyped-def]
    harness_graph_id: str,
    task_store,  # type: ignore[no-untyped-def]
) -> list[ContextBlock]:
    out: list[ContextBlock] = []
    for dep_id in needs:
        dep = task_store.get_task(dep_id)
        if dep is None:
            continue
        summaries = dep.get("summaries") or []
        text = _format_dependency_summaries(summaries)
        out.append(
            ContextBlock(
                kind=ContextBlockKind.DEPENDENCY_SUMMARY,
                priority=ContextPriority.MEDIUM,
                text=text,
                source_id=dep_id,
                source_kind="task_center_task",
                metadata={"dep_id": dep_id},
            )
        )
    return out


def _format_dependency_summaries(summaries: list[dict]) -> str:  # type: ignore[type-arg]
    if not summaries:
        return "(no summary recorded)"
    last = summaries[-1]
    if not isinstance(last, dict):
        return str(last)
    summary_text = last.get("summary") or last.get("outcome") or "(empty)"
    return str(summary_text)


GENERATOR_V1_RECIPE = ContextRecipe(
    id=GENERATOR_V1,
    required_scope_fields=_REQUIRED_FIELDS,
    build=_generator_v1_build,
)


__all__ = ["GENERATOR_V1", "GENERATOR_V1_RECIPE"]

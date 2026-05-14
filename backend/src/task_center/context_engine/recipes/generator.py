"""``generator_v1`` recipe — context for one generator task spawn.

Emits the current attempt plan, dependency results, and the assigned local task
in presentation order. The assigned task is required but remains last so the
generator ends on its concrete obligation.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

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
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.scope import ContextScope

if TYPE_CHECKING:
    from db.stores.task_center_store import TaskCenterStore

GENERATOR_V1 = "generator_v1"
_REQUIRED_FIELDS = frozenset({"mission_id", "attempt_id", "task_id"})


def _generator_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    # Engine pre-validates required scope fields via ``assert_fields``; this
    # explicit guard makes the recipe self-defending under ``python -O`` where
    # ``assert`` would be stripped.
    if (
        scope.mission_id is None
        or scope.attempt_id is None
        or scope.task_id is None
    ):
        raise ContextEngineError(
            "generator_v1 requires mission_id, attempt_id, and task_id; "
            f"got {scope!r}"
        )
    attempt = deps.attempt_store.get(scope.attempt_id)
    if attempt is None:
        raise ContextEngineError(
            f"Attempt {scope.attempt_id!r} not found"
        )
    task = deps.task_store.get_task(scope.task_id)
    if task is None:
        raise ContextEngineError(
            f"TaskCenterTask {scope.task_id!r} not found"
        )

    blocks: list[ContextBlock] = []
    if attempt.task_specification:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.TASK_SPECIFICATION,
                priority=ContextPriority.HIGH,
                text=attempt.task_specification,
                source_id=attempt.id,
                source_kind="attempt",
            )
        )

    blocks.extend(
        _dependency_summary_blocks(
            needs=tuple(str(dep) for dep in task.get("needs") or ()),
            task_store=deps.task_store,
        )
    )
    blocks.append(
        ContextBlock(
            kind=ContextBlockKind.PLANNED_TASK_SPEC,
            priority=ContextPriority.REQUIRED,
            text=str(task.get("rendered_prompt") or ""),
            source_id=scope.task_id,
            source_kind="task_center_task",
        )
    )

    return ContextPacket(
        target_role="generator",
        target_id=scope.task_id,
        canonical_refs=ContextRefs(
            mission_id=scope.mission_id,
            episode_id=scope.episode_id or attempt.episode_id,
            attempt_id=scope.attempt_id,
            task_id=scope.task_id,
        ),
        blocks=blocks,
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


def _dependency_summary_blocks(
    *,
    needs: tuple[str, ...],
    task_store: TaskCenterStore,
) -> list[ContextBlock]:
    out: list[ContextBlock] = []
    for dep_id in needs:
        dep = task_store.get_task(dep_id)
        if dep is None:
            # ``needs`` are persisted DAG edges validated at planner-submission
            # acceptance; a missing row here is a harness invariant violation,
            # not a tolerable absence. Surface it so the LLM never reasons
            # over a silently-truncated dependency frame.
            raise ContextEngineError(
                f"Dependency task {dep_id!r} referenced by needs is missing; "
                "generator context cannot be assembled without dependency results."
            )
        out.append(
            ContextBlock(
                kind=ContextBlockKind.DEPENDENCY_SUMMARY,
                priority=ContextPriority.MEDIUM,
                text=latest_summary_text(dep.get("summaries")),
                source_id=dep_id,
                source_kind="task_center_task",
                metadata={
                    "dep_id": dep_id,
                    "group_heading": "# Dependency Results",
                    "subheading": str(dep.get("id") or dep_id),
                },
            )
        )
    return out


GENERATOR_V1_RECIPE = ContextRecipe(
    id=GENERATOR_V1,
    required_scope_fields=_REQUIRED_FIELDS,
    build=_generator_v1_build,
)

"""``entry_executor_v1`` recipe — context for the top-level entry executor.

Emits one ``entry_request`` block (priority=required) sourced from the
entry task row's ``task_input``. No ``complex_task_summary`` block — that
only ships at request close, after this recipe's lifetime.
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

ENTRY_EXECUTOR_V1 = "entry_executor_v1"
_REQUIRED_FIELDS = frozenset({"request_id", "task_id"})


def _entry_executor_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    task = deps.task_store.get_task(scope.task_id)
    if task is None:
        raise ContextEngineError(
            f"Entry task {scope.task_id!r} not found"
        )
    text = str(task.get("task_input") or "")
    block = ContextBlock(
        kind=ContextBlockKind.ENTRY_REQUEST,
        priority=ContextPriority.REQUIRED,
        text=text,
        source_id=scope.task_id,
        source_kind="task_center_task",
    )
    return ContextPacket(
        target_role="executor",
        target_id=scope.task_id,
        canonical_refs=ContextRefs(
            request_id=scope.request_id,
            task_id=scope.task_id,
        ),
        blocks=[block],
        source_ids=[scope.task_id],
    )


ENTRY_EXECUTOR_V1_RECIPE = ContextRecipe(
    id=ENTRY_EXECUTOR_V1,
    required_scope_fields=_REQUIRED_FIELDS,
    build=_entry_executor_v1_build,
)


__all__ = ["ENTRY_EXECUTOR_V1", "ENTRY_EXECUTOR_V1_RECIPE"]

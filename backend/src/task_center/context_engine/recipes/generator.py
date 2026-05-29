"""``generator`` recipe — context for one generator task spawn.

Emits the current attempt's ``<plan_spec>``, dependency outputs wrapped in a
``<dependency>`` group, and the assigned local task. XML shape:

* ``<plan_spec>`` — standalone block (no surrounding wrapper).
* ``<dependency>`` group with one ``<task id="..." status="...">`` child per
  upstream task, omitted when the assigned task has no deps.
* ``<assigned_task task_id="...">`` — the generator's local contract, anchored
  last so the agent ends on its concrete obligation.

The planner-only ``<deferred_goal_for_next_iteration>`` is intentionally
absent: it is a planner / evaluator concern and would distract executors.

The ``<Task Guidance>`` row is assembled at launch time by
``AgentEntryComposer`` via the registry-driven
``task_center/context_engine/task_guidance.py:build_task_guidance``.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from task_center.context_engine.exceptions import ContextEngineError
from task_center.context_engine.core import ContextEngineDeps
from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center._core.generator_summaries import task_outcome_from_row
from task_center.context_engine.recipes._task_xml import block_task_body, task_attrs
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.scope import ContextScope

if TYPE_CHECKING:
    from task_center._core.persistence import TaskStoreProtocol


GENERATOR_ID = "generator"
_REQUIRED_FIELDS = frozenset({"workflow_id", "attempt_id", "task_id"})


def build_generator_context(scope: ContextScope, deps: ContextEngineDeps) -> ContextPacket:
    attempt_id = scope.require_field("attempt_id")
    task_id = scope.require_field("task_id")
    workflow_id = scope.require_field("workflow_id")

    attempt = deps.attempt_store.get(attempt_id)
    if attempt is None:
        raise ContextEngineError(f"Attempt {attempt_id!r} not found")
    iteration_id = scope.iteration_id or attempt.iteration_id
    task = deps.task_store.get_task(task_id)
    if task is None:
        raise ContextEngineError(f"TaskCenterTask {task_id!r} not found")

    blocks: list[ContextBlock] = []
    if attempt.plan_spec:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.TASK_SPECIFICATION,
                priority=ContextPriority.HIGH,
                text=attempt.plan_spec,
                source_id=attempt.id,
                source_kind="attempt",
                metadata={"tag": "plan_spec"},
            )
        )

    needs = tuple(str(dep) for dep in task.get("needs") or ())
    blocks.extend(_dependency_blocks(needs=needs, task_store=deps.task_store))
    blocks.append(
        ContextBlock(
            kind=ContextBlockKind.PLANNED_TASK_SPEC,
            priority=ContextPriority.REQUIRED,
            text=str(task.get("context_message") or ""),
            source_id=task_id,
            source_kind="task_center_task",
            metadata={
                "tag": "assigned_task",
                "attrs": f'task_id="{task_id}"',
            },
        )
    )

    return ContextPacket(
        target_role="generator",
        target_id=task_id,
        canonical_refs=ContextRefs(
            workflow_id=workflow_id,
            iteration_id=iteration_id or attempt.iteration_id,
            attempt_id=attempt_id,
            task_id=task_id,
        ),
        blocks=blocks,
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


_DEPENDENCY_GROUP_ID = "dependencies"


def _dependency_blocks(
    *,
    needs: tuple[str, ...],
    task_store: TaskStoreProtocol,
) -> list[ContextBlock]:
    """Emit a ``<dependency>`` group with one ``<task>`` child per upstream task."""
    if not needs:
        return []
    out: list[ContextBlock] = []
    for dep_id in needs:
        dep = task_store.get_task(dep_id)
        if dep is None:
            # ``needs`` are persisted DAG edges validated at planner-submission
            # acceptance; a missing row here is a harness invariant violation.
            raise ContextEngineError(
                f"Dependency task {dep_id!r} referenced by needs is missing; "
                "generator context cannot be assembled without dependency results."
            )
        outcome = task_outcome_from_row(dep_id, dep)
        text, pre_rendered = block_task_body(outcome)
        metadata = {
            "group_id": _DEPENDENCY_GROUP_ID,
            "group_tag": "dependency",
            "child_tag": "task",
            "attrs": task_attrs(outcome),
        }
        if pre_rendered:
            metadata["pre_rendered_xml"] = "true"
        out.append(
            ContextBlock(
                kind=ContextBlockKind.DEPENDENCY_SUMMARY,
                priority=ContextPriority.MEDIUM,
                text=text,
                source_id=dep_id,
                source_kind="task_center_task",
                metadata=metadata,
            )
        )
    return out


GENERATOR_RECIPE = ContextRecipe(
    id=GENERATOR_ID,
    required_scope_fields=_REQUIRED_FIELDS,
    build=build_generator_context,
)

"""``evaluator`` recipe — context for one evaluator spawn.

Emits the current attempt's substance as flat top-level blocks — ``<plan_spec>``
(framing) + one ``<task id status>`` per generator task (summary-only) +
``<evaluation_criteria>`` (the authority). No goal / iteration frame, no
prior-iteration background, no failed-prior attempts, and no
``<deferred_goal_for_next_iteration>``: the evaluator's job is a binary verdict
on *this* attempt against *these* criteria, so anything outside the current
attempt is planner-scope or retry-fuel, not evaluation evidence.
"""

from __future__ import annotations

from task_center.context_engine.exceptions import ContextEngineError
from task_center.context_engine.core import ContextEngineDeps
from task_center.context_engine.packet import (
    ContextPacket,
    ContextRefs,
)
from task_center.context_engine.recipes.attempts import (
    current_attempt_flat_blocks,
)
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.scope import ContextScope

EVALUATOR_ID = "evaluator"
_REQUIRED_FIELDS = frozenset({"attempt_id"})


def build_evaluator_context(scope: ContextScope, deps: ContextEngineDeps) -> ContextPacket:
    attempt_id = scope.require_field("attempt_id")

    attempt = deps.attempt_store.get(attempt_id)
    if attempt is None:
        raise ContextEngineError(f"Attempt {attempt_id!r} not found")

    blocks = current_attempt_flat_blocks(attempt=attempt, task_store=deps.task_store)

    return ContextPacket(
        target_role="evaluator",
        target_id=attempt_id,
        canonical_refs=ContextRefs(
            workflow_id=scope.workflow_id,
            iteration_id=attempt.iteration_id,
            attempt_id=attempt_id,
        ),
        blocks=blocks,
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


EVALUATOR_RECIPE = ContextRecipe(
    id=EVALUATOR_ID,
    required_scope_fields=_REQUIRED_FIELDS,
    build=build_evaluator_context,
)

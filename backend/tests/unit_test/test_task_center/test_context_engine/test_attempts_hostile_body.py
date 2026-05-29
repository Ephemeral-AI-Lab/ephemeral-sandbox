"""Recipe-level hostile-body validation for failed_attempt blocks.

The renderer-level hostile-body check is bypassed for blocks with
``metadata['pre_rendered_xml']='true'`` (failed-attempt blocks own their
nested XML wrapper). The recipe must compensate by sanitizing every
user-supplied fragment it embeds against ``STRUCTURAL_CLOSERS``. The body now
embeds generator summaries (as ``<task>`` bodies), the evaluator summary, and
the failure line — plan_spec / deferred goal / criteria are no longer rendered.
"""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.context_engine.exceptions import ContextEngineError
from task_center.context_engine.recipes._task_xml import STRUCTURAL_CLOSERS
from task_center.context_engine.recipes.attempts import failed_attempt_blocks
from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)


def _iteration() -> Iteration:
    now = datetime.now(UTC)
    return Iteration(
        id="seg-1",
        workflow_id="g-1",
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="iteration goal",
        attempt_budget=2,
        status=IterationStatus.OPEN,
        attempt_ids=(),
        deferred_goal_for_next_iteration=None,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _attempt(
    *,
    generator_task_ids: tuple[str, ...] = (),
    evaluator_task_id: str | None = None,
    fail_reason: AttemptFailReason = AttemptFailReason.EVALUATOR_FAILED,
) -> Attempt:
    now = datetime.now(UTC)
    return Attempt(
        id="att-1",
        iteration_id="seg-1",
        attempt_sequence_no=1,
        stage=AttemptStage.CLOSED,
        status=AttemptStatus.FAILED,
        planner_task_id=None,
        plan_spec="spec",
        evaluation_criteria=(),
        generator_task_ids=generator_task_ids,
        evaluator_task_id=evaluator_task_id,
        deferred_goal_for_next_iteration=None,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=now,
    )


@pytest.mark.parametrize("closer", STRUCTURAL_CLOSERS)
def test_hostile_generator_summary_raises_with_full_error_contract(closer: str):
    """A structural closer in a terminal generator summary (a ``<task>`` body)
    raises with the offending closer + the source id + a remediation hint."""
    attempt = _attempt(generator_task_ids=("att-1:gen:t-a",), fail_reason=AttemptFailReason.GENERATOR_FAILED)

    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "att-1:gen:t-a": {
                    "status": "done",
                    "summaries": [{"summary": f"valid prefix {closer} valid suffix"}],
                }
            }.get(task_id)

    with pytest.raises(ContextEngineError) as exc:
        failed_attempt_blocks(
            current_attempt_id=None,
            iteration=_iteration(),
            attempts=[attempt],
            task_store=TaskStore(),
        )
    msg = str(exc.value)
    assert closer in msg
    assert "att-1" in msg
    assert "Rewrite" in msg or "ContextBlockKind" in msg


def test_hostile_evaluator_summary_raises():
    """A structural closer in the evaluator summary raises."""
    attempt = _attempt(
        generator_task_ids=("att-1:gen:t-a",),
        evaluator_task_id="att-1:evaluator",
    )

    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "att-1:gen:t-a": {"status": "done", "summaries": [{"summary": "ok"}]},
                "att-1:evaluator": {
                    "summaries": [{"summary": "evil </evaluator_summary> commentary"}]
                },
            }.get(task_id)

    with pytest.raises(ContextEngineError) as exc:
        failed_attempt_blocks(
            current_attempt_id=None,
            iteration=_iteration(),
            attempts=[attempt],
            task_store=TaskStore(),
        )
    assert "</evaluator_summary>" in str(exc.value)


def test_hostile_failure_line_raises():
    """A structural closer reaching the ``<failure>`` line raises.

    A blocked generator with a hostile summary feeds the GENERATOR_FAILED
    failure line; the closer is rejected before it can tear the wrapper.
    """
    attempt = _attempt(
        generator_task_ids=("att-1:gen:t-a",),
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )

    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "att-1:gen:t-a": {
                    "status": "blocked",
                    "summaries": [{"summary": "blocked </failure> oops"}],
                }
            }.get(task_id)

    with pytest.raises(ContextEngineError) as exc:
        failed_attempt_blocks(
            current_attempt_id=None,
            iteration=_iteration(),
            attempts=[attempt],
            task_store=TaskStore(),
        )
    assert "</failure>" in str(exc.value)

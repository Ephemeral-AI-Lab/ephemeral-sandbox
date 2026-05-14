"""Direct tests for failed attempt landscape helper behavior."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.context_engine.packet import ContextPriority
from task_center.context_engine.recipes.attempt_landscape import (
    failed_attempt_landscape_blocks,
)
from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)


def _attempt(
    sequence_no: int,
    *,
    attempt_id: str | None = None,
    status: AttemptStatus = AttemptStatus.FAILED,
    task_specification: str | None = None,
    evaluation_criteria: tuple[str, ...] = (),
    generator_task_ids: tuple[str, ...] = (),
    evaluator_task_id: str | None = None,
    continuation_goal: str | None = None,
    fail_reason: AttemptFailReason | None = None,
) -> Attempt:
    now = datetime.now(UTC)
    return Attempt(
        id=attempt_id or f"attempt-{sequence_no}",
        episode_id="seg-1",
        attempt_sequence_no=sequence_no,
        stage=AttemptStage.CLOSED,
        status=status,
        planner_task_id=None,
        task_specification=task_specification,
        evaluation_criteria=evaluation_criteria,
        generator_task_ids=generator_task_ids,
        evaluator_task_id=evaluator_task_id,
        continuation_goal=continuation_goal,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=now,
    )


def test_excludes_current_attempt_even_if_current_is_failed():
    current = _attempt(
        3,
        attempt_id="current",
        task_specification="current spec",
        evaluation_criteria=("current crit",),
        fail_reason=AttemptFailReason.PLANNER_FAILED,
    )
    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=current.id,
        attempts=[
            current,
            _attempt(
                2,
                task_specification="older spec",
                evaluation_criteria=("older crit",),
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            ),
            _attempt(4, status=AttemptStatus.RUNNING),
            _attempt(
                1,
                task_specification="oldest spec",
                evaluation_criteria=("oldest crit",),
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            ),
        ],
    )

    assert [block.source_id for block in blocks] == ["attempt-1", "attempt-2"]
    assert all(block.priority == ContextPriority.HIGH for block in blocks)


def test_renders_missing_spec_empty_criteria_and_unknown_reason():
    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[_attempt(1)],
    )

    assert len(blocks) == 1
    assert "### Accepted Plan" in blocks[0].text
    assert "Plan type: unsubmitted" in blocks[0].text
    assert "Specification:\n(not submitted)" in blocks[0].text
    assert "### Generator Outcomes" in blocks[0].text
    assert "Status summary:\n- (no generator tasks recorded)" in blocks[0].text
    assert "continuation_goal" not in blocks[0].text
    assert "fail_reason" not in blocks[0].text


def test_renders_plan_kind_statuses_and_generator_summaries():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {
                    "status": "done",
                    "summaries": [
                        {"summary": "first summary"},
                        {"summary": "built catalog slice"},
                    ]
                },
                "t-b": {
                    "status": "done",
                    "summaries": [{"outcome": "verified checkout"}],
                },
            }.get(task_id)

    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                1,
                task_specification="partial spec",
                evaluation_criteria=("criterion",),
                generator_task_ids=("t-a", "t-b", "t-missing"),
                continuation_goal="continue with admin tools",
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )

    assert len(blocks) == 1
    assert "Plan type: partial" in blocks[0].text
    assert "continue with admin tools" not in blocks[0].text
    assert "- t-a: done" in blocks[0].text
    assert "- t-b: done" in blocks[0].text
    assert "- t-missing: missing task row" in blocks[0].text
    assert "#### t-a\n\nbuilt catalog slice" in blocks[0].text
    assert "#### t-b\n\nverified checkout" in blocks[0].text


def test_renders_full_plan_kind_for_submitted_nonpartial_attempt():
    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                1,
                task_specification="submitted spec",
                evaluation_criteria=("criterion",),
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            )
        ],
    )

    assert "Plan type: full" in blocks[0].text


def test_evaluator_failure_renders_evaluator_judgment():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {
                    "status": "done",
                    "summaries": [{"summary": "generator completed"}],
                },
                "eval-1": {
                    "summaries": [
                        {"summary": "older evaluator note"},
                        {
                            "summary": (
                                "checkout review displayed 3197 before submit "
                                "\nwhile confirmation displayed 3411"
                            )
                        },
                    ]
                }
            }.get(task_id)

    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                1,
                task_specification="submitted spec",
                evaluation_criteria=("total criterion",),
                generator_task_ids=("t-a",),
                evaluator_task_id="eval-1",
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )

    assert "### Evaluator Judgment" in blocks[0].text
    assert "Evaluation criteria:\n  - total criterion" in blocks[0].text
    assert (
        "Evaluator summary:\ncheckout review displayed 3197 before submit"
        in blocks[0].text
    )
    assert "fail_reason" not in blocks[0].text


def test_generator_failure_hides_evaluator_and_keeps_blocked_task_in_status_only():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {
                    "status": "done",
                    "summaries": [{"summary": "completed dependency"}],
                },
                "t-b": {
                    "status": "failed",
                    "summaries": [{"summary": "failed after partial edit"}],
                },
                "t-c": {
                    "status": "blocked",
                    "summaries": [{"blocked_by": "t-b"}],
                },
                "eval-1": {
                    "summaries": [{"summary": "should not render"}],
                },
            }.get(task_id)

    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                1,
                task_specification="submitted spec",
                evaluation_criteria=("criterion",),
                generator_task_ids=("t-a", "t-b", "t-c"),
                evaluator_task_id="eval-1",
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )

    text = blocks[0].text
    assert "- t-a: done" in text
    assert "- t-b: failed" in text
    assert "- t-c: blocked by t-b" in text
    assert "#### t-a\n\ncompleted dependency" in text
    assert "#### t-b\n\nfailed after partial edit" in text
    assert "#### t-c" not in text
    assert "### Evaluator Judgment" not in text
    assert "should not render" not in text


def test_generator_summaries_include_every_task_in_failed_attempt():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "status": "done",
                "summaries": [{"summary": f"summary for {task_id}"}],
            }

    task_ids = tuple(f"t-{i}" for i in range(14))

    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                1,
                task_specification="spec",
                generator_task_ids=task_ids,
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )

    for task_id in task_ids:
        assert f"- {task_id}: done" in blocks[0].text
        assert f"#### {task_id}" in blocks[0].text
    assert "generator summaries omitted" not in blocks[0].text


def test_generator_summary_text_is_not_truncated():
    class TaskStore:
        def get_task(self, task_id: str):
            return {"status": "done", "summaries": [{"summary": "x" * 850}]}

    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                1,
                task_specification="spec",
                generator_task_ids=("t-a",),
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )

    assert "x" * 850 in blocks[0].text
    assert "truncated" not in blocks[0].text


def test_all_failed_attempts_render_in_sequence_order():
    blocks = failed_attempt_landscape_blocks(
        current_attempt_id=None,
        attempts=[
            _attempt(
                sequence_no,
                task_specification=f"spec-{sequence_no}",
                evaluation_criteria=(f"crit-{sequence_no}",),
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            )
            for sequence_no in range(8, 0, -1)
        ],
    )

    assert [block.metadata["attempt_sequence_no"] for block in blocks] == [
        str(sequence_no) for sequence_no in range(1, 9)
    ]
    assert all(block.priority == ContextPriority.HIGH for block in blocks)
    assert all("truncated_count" not in block.metadata for block in blocks)
    assert all("omitted" not in block.text for block in blocks)

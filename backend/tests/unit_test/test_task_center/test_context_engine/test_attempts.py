"""Direct tests for the ``<attempt>`` emitters (XML body + metadata)."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.context_engine.packet import ContextPriority
from task_center.context_engine.recipes.attempts import (
    current_attempt_flat_blocks,
    failed_attempt_blocks,
)
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


def _iteration(sequence_no: int = 1) -> Iteration:
    now = datetime.now(UTC)
    return Iteration(
        id="seg-1",
        goal_id="g-1",
        sequence_no=sequence_no,
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
    sequence_no: int,
    *,
    attempt_id: str | None = None,
    status: AttemptStatus = AttemptStatus.FAILED,
    plan_spec: str | None = None,
    evaluation_criteria: tuple[str, ...] = (),
    generator_task_ids: tuple[str, ...] = (),
    evaluator_task_id: str | None = None,
    deferred_goal_for_next_iteration: str | None = None,
    fail_reason: AttemptFailReason | None = None,
) -> Attempt:
    now = datetime.now(UTC)
    return Attempt(
        id=attempt_id or f"attempt-{sequence_no}",
        iteration_id="seg-1",
        attempt_sequence_no=sequence_no,
        stage=AttemptStage.CLOSED,
        status=status,
        planner_task_id=None,
        plan_spec=plan_spec,
        evaluation_criteria=evaluation_criteria,
        generator_task_ids=generator_task_ids,
        evaluator_task_id=evaluator_task_id,
        deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=now,
    )


def test_excludes_current_attempt_even_if_current_is_failed():
    current = _attempt(
        3,
        attempt_id="current",
        plan_spec="current spec",
        evaluation_criteria=("current crit",),
        fail_reason=AttemptFailReason.PLANNER_FAILED,
    )
    blocks = failed_attempt_blocks(
        current_attempt_id=current.id,
        iteration=_iteration(),
        attempts=[
            current,
            _attempt(
                2,
                plan_spec="older spec",
                evaluation_criteria=("older crit",),
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            ),
            _attempt(4, status=AttemptStatus.RUNNING),
            _attempt(
                1,
                plan_spec="oldest spec",
                evaluation_criteria=("oldest crit",),
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            ),
        ],
    )

    assert [block.source_id for block in blocks] == ["attempt-1", "attempt-2"]
    assert all(block.priority == ContextPriority.HIGH for block in blocks)


def test_prior_attempt_block_metadata_carries_status_and_verdict():
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(sequence_no=3),
        attempts=[
            _attempt(1, plan_spec="spec1", fail_reason=AttemptFailReason.PLANNER_FAILED),
        ],
    )
    block = blocks[0]
    assert block.metadata["group_id"] == "iteration_3_current"
    assert block.metadata["group_tag"] == "iteration"
    assert block.metadata["group_attrs"] == 'iteration_no="3" status="current"'
    assert block.metadata["child_tag"] == "attempt"
    assert block.metadata["attrs"] == (
        'attempt_no="1" status="prior" verdict="fail"'
    )


def test_prior_attempt_body_emits_flat_plan_spec_child():
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[_attempt(1, plan_spec="submitted spec")],
    )
    body = blocks[0].text
    assert "<plan_spec>\nsubmitted spec\n</plan_spec>" in body
    # No <attempt_plan> wrapper anymore.
    assert "<attempt_plan>" not in body
    assert "</attempt_plan>" not in body
    assert "<deferred_goal_for_next_iteration>" not in body, (
        "absent deferred goal must not produce a child element"
    )


def test_prior_attempt_body_emits_deferred_goal_when_present():
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(
                1,
                plan_spec="partial spec",
                deferred_goal_for_next_iteration="continue with admin tools",
            )
        ],
    )
    body = blocks[0].text
    assert "<plan_spec>\npartial spec\n</plan_spec>" in body
    assert (
        "<deferred_goal_for_next_iteration>\ncontinue with admin tools\n"
        "</deferred_goal_for_next_iteration>"
    ) in body


def test_planner_failed_renders_compact_bypassed_body():
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(1, fail_reason=AttemptFailReason.PLANNER_FAILED)
        ],
    )
    body = blocks[0].text
    assert body == (
        '<plan_spec status="unsubmitted"/>\n'
        '<status_summary status="not_started"/>\n'
        '<evaluator_summary status="bypassed" reason="planner_failed"/>'
    )


def test_startup_failed_renders_compact_bypassed_body():
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(1, fail_reason=AttemptFailReason.STARTUP_FAILED)
        ],
    )
    body = blocks[0].text
    assert body == (
        '<plan_spec status="unsubmitted"/>\n'
        '<status_summary status="not_started"/>\n'
        '<evaluator_summary status="bypassed" reason="startup_failed"/>'
    )


def test_prior_body_emits_status_summary_and_task_children():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {
                    "status": "done",
                    "summaries": [
                        {"summary": "first summary"},
                        {"summary": "built catalog slice"},
                    ],
                },
                "t-b": {
                    "status": "done",
                    "summaries": [{"outcome": "verified checkout"}],
                },
            }.get(task_id)

    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(
                1,
                plan_spec="partial spec",
                evaluation_criteria=("criterion",),
                generator_task_ids=("t-a", "t-b", "t-missing"),
                deferred_goal_for_next_iteration="continue with admin tools",
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )

    body = blocks[0].text
    # Wrappers are gone; status_summary + task siblings sit at attempt scope.
    assert "<generator_outcomes>" not in body
    assert "</generator_outcomes>" not in body
    assert "<status_summary>" in body
    assert "t-a: done" in body
    assert "t-b: done" in body
    assert "t-missing: missing task row" in body
    assert (
        '<task id="t-a" status="done">\nbuilt catalog slice\n</task>'
    ) in body
    assert (
        '<task id="t-b" status="done">\nverified checkout\n</task>'
    ) in body


def test_prior_body_bypassed_on_generator_failure():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {"status": "done", "summaries": [{"summary": "ok"}]},
                "t-b": {"status": "failed", "summaries": [{"summary": "boom"}]},
                "eval-1": {"summaries": [{"summary": "should not be read"}]},
            }.get(task_id)

    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(
                1,
                plan_spec="spec",
                evaluation_criteria=("c1",),
                generator_task_ids=("t-a", "t-b"),
                evaluator_task_id="eval-1",
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )
    body = blocks[0].text
    assert (
        '<evaluator_summary status="bypassed" reason="generator_failed">'
    ) in body
    assert "task(s) failed: t-b" in body
    assert "should not be read" not in body


def test_prior_body_renders_evaluator_children_with_failed_criteria():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {"status": "done", "summaries": [{"summary": "generator ok"}]},
                "eval-1": {
                    "summaries": [
                        {
                            "summary": "checkout review failed total mismatch",
                            "payload": {"failed_criteria": ["total"]},
                        }
                    ]
                },
            }.get(task_id)

    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(
                1,
                plan_spec="spec",
                evaluation_criteria=("total",),
                generator_task_ids=("t-a",),
                evaluator_task_id="eval-1",
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )
    body = blocks[0].text
    # No <evaluator_judgment> wrapper.
    assert "<evaluator_judgment" not in body
    assert "</evaluator_judgment>" not in body
    assert "<evaluation_criteria>\ntotal\n</evaluation_criteria>" in body
    assert "checkout review failed total mismatch" in body
    assert "<failed_criteria>\ntotal\n</failed_criteria>" in body


def test_prior_body_includes_passed_criteria_when_payload_carries_them():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {"status": "done", "summaries": [{"summary": "generator ok"}]},
                "eval-1": {
                    "summaries": [
                        {
                            "outcome": "success",
                            "summary": "passing summary",
                            "payload": {"passed_criteria": ["c1", "c2"]},
                        }
                    ]
                },
            }.get(task_id)

    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(
                1,
                plan_spec="spec",
                evaluation_criteria=("c1", "c2"),
                generator_task_ids=("t-a",),
                evaluator_task_id="eval-1",
                fail_reason=AttemptFailReason.EVALUATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )
    body = blocks[0].text
    assert "<passed_criteria>\nc1\nc2\n</passed_criteria>" in body


def test_all_failed_attempts_render_in_sequence_order():
    attempts = [
        _attempt(3, plan_spec="spec3", fail_reason=AttemptFailReason.PLANNER_FAILED),
        _attempt(1, plan_spec="spec1", fail_reason=AttemptFailReason.PLANNER_FAILED),
        _attempt(2, plan_spec="spec2", fail_reason=AttemptFailReason.PLANNER_FAILED),
    ]
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=attempts,
    )
    assert [b.source_id for b in blocks] == ["attempt-1", "attempt-2", "attempt-3"]
    assert [b.metadata["attrs"] for b in blocks] == [
        'attempt_no="1" status="prior" verdict="fail"',
        'attempt_no="2" status="prior" verdict="fail"',
        'attempt_no="3" status="prior" verdict="fail"',
    ]


# ---------------------------------------------------------------------------
# Current attempt block (flat — evaluator-only).
# ---------------------------------------------------------------------------


def test_current_attempt_flat_blocks_emit_plan_spec_and_criteria():
    blocks = current_attempt_flat_blocks(
        attempt=_attempt(
            2,
            status=AttemptStatus.RUNNING,
            plan_spec="active spec",
            evaluation_criteria=("crit-a",),
        ),
    )
    assert [b.metadata["tag"] for b in blocks] == ["plan_spec", "evaluation_criteria"]
    plan_spec, criteria = blocks
    # Flat top-level blocks — no <iteration>/<attempt> wrapper, no pre-render.
    assert "group_tag" not in plan_spec.metadata
    assert "pre_rendered_xml" not in plan_spec.metadata
    assert plan_spec.priority == ContextPriority.HIGH
    assert plan_spec.text == "active spec"
    assert criteria.priority == ContextPriority.REQUIRED
    assert criteria.text == "crit-a"


def test_current_attempt_flat_blocks_emit_one_task_block_per_generator():
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {"status": "done", "summaries": [{"summary": "built slice"}]},
                "t-b": {"status": "done", "summaries": [{"summary": "(empty)"}]},
            }.get(task_id)

    blocks = current_attempt_flat_blocks(
        attempt=_attempt(
            1,
            status=AttemptStatus.RUNNING,
            plan_spec="spec",
            evaluation_criteria=("c1",),
            generator_task_ids=("t-a", "t-b"),
        ),
        task_store=TaskStore(),
    )
    task_blocks = [b for b in blocks if b.metadata.get("tag") == "task"]
    assert [b.metadata["attrs"] for b in task_blocks] == [
        'id="t-a" status="done"',
        'id="t-b" status="done"',
    ]
    # Real summary becomes the body; a placeholder summary collapses to empty.
    assert task_blocks[0].text == "built slice"
    assert task_blocks[1].text == ""


def test_current_attempt_flat_blocks_omit_deferred_goal():
    """Even a defers-goal attempt emits no deferred block — the evaluator
    judges the current slice against its criteria, not the remainder."""
    blocks = current_attempt_flat_blocks(
        attempt=_attempt(
            2,
            status=AttemptStatus.RUNNING,
            plan_spec="partial",
            deferred_goal_for_next_iteration="next slice",
        ),
    )
    assert [b.metadata["tag"] for b in blocks] == ["plan_spec"]
    for block in blocks:
        assert "deferred_goal_for_next_iteration" not in block.text
        assert "has_deferred_goal_for_next_iteration" not in block.metadata


def test_current_attempt_flat_blocks_omitted_without_plan_spec():
    assert (
        current_attempt_flat_blocks(
            attempt=_attempt(1, plan_spec=None, status=AttemptStatus.RUNNING),
        )
        == []
    )

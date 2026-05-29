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
        workflow_id="g-1",
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


def test_prior_attempt_block_metadata_carries_attempt_no_only():
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
    # Group attribute renamed status -> position.
    assert block.metadata["group_attrs"] == 'iteration_no="3" position="current"'
    assert block.metadata["child_tag"] == "attempt"
    # attrs is now attempt_no only — no status="prior" verdict="fail" (the
    # attempt is a prior attempt *of the current iteration*).
    assert block.metadata["attrs"] == 'attempt_no="1"'


def test_prior_attempt_body_omits_plan_spec_child():
    # Repurposed: <plan_spec> was DROPPED from the failed-attempt body. With no
    # generators and fail_reason=None the body is just the <failure> line.
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[_attempt(1, plan_spec="submitted spec")],
    )
    body = blocks[0].text
    assert "<plan_spec>" not in body
    assert "submitted spec" not in body
    assert body == "<failure>\n(no detail recorded)\n</failure>"


def test_prior_attempt_body_omits_deferred_goal():
    # Repurposed: <deferred_goal_for_next_iteration> was DROPPED from the
    # failed-attempt body even when the attempt carries one.
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
    assert "<deferred_goal_for_next_iteration>" not in body
    assert "continue with admin tools" not in body
    assert "<plan_spec>" not in body
    assert body == "<failure>\n(no detail recorded)\n</failure>"


def test_planner_failed_renders_failure_only_body():
    # The compact "bypassed" body (plan_spec/status_summary/evaluator_summary
    # self-closers) was DROPPED. PLANNER_FAILED with no generators now renders
    # just the <failure> line: "planner: <summary>" (no store -> no detail).
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(1, fail_reason=AttemptFailReason.PLANNER_FAILED)
        ],
    )
    body = blocks[0].text
    assert body == "<failure>\nplanner: (no detail recorded)\n</failure>"


def test_startup_failed_renders_failure_only_body():
    # The compact "bypassed" body was DROPPED. STARTUP_FAILED now renders just
    # the <failure> line "agent_launch_failed".
    blocks = failed_attempt_blocks(
        current_attempt_id=None,
        iteration=_iteration(),
        attempts=[
            _attempt(1, fail_reason=AttemptFailReason.STARTUP_FAILED)
        ],
    )
    body = blocks[0].text
    assert body == "<failure>\nagent_launch_failed\n</failure>"


def test_prior_body_emits_terminal_task_children():
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
    # <status_summary> was DROPPED; only terminal <task> children + <failure>
    # remain. done->success; the missing (un-started) row is excluded entirely
    # (terminal-only). No evaluator ran -> failure line carries no detail.
    assert "<status_summary>" not in body
    assert "<generator_outcomes>" not in body
    assert 'id="t-missing"' not in body
    assert body == (
        '<task id="t-a" status="success">\n'
        "built catalog slice\n"
        "</task>\n"
        '<task id="t-b" status="success">\n'
        "verified checkout\n"
        "</task>\n"
        "<failure>\n"
        "evaluator: (no detail recorded)\n"
        "</failure>"
    )


def test_prior_body_renders_terminal_tasks_and_generator_failure():
    # The "bypassed" evaluator_summary message was DROPPED. A GENERATOR_FAILED
    # attempt never produced an evaluator summary (so the fixture sets no
    # evaluator_task_id, matching the ground-truth GENERATOR_FAILED shape).
    # Body = terminal <task>s + <failure>"generator <local_id>: ...".
    class TaskStore:
        def get_task(self, task_id: str):
            return {
                "t-a": {"status": "done", "summaries": [{"summary": "ok"}]},
                "t-b": {"status": "failed", "summaries": [{"summary": "boom"}]},
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
                fail_reason=AttemptFailReason.GENERATOR_FAILED,
            )
        ],
        task_store=TaskStore(),
    )
    body = blocks[0].text
    assert "<evaluator_summary>" not in body
    assert body == (
        '<task id="t-a" status="success">\n'
        "ok\n"
        "</task>\n"
        '<task id="t-b" status="failure">\n'
        "boom\n"
        "</task>\n"
        "<failure>\n"
        "generator t-b: boom\n"
        "</failure>"
    )


def test_prior_body_renders_evaluator_summary_on_evaluator_failure():
    # <evaluation_criteria>/<failed_criteria>/<passed_criteria> elements were
    # DROPPED from the failed-attempt body. An EVALUATOR_FAILED attempt whose
    # evaluator ran now renders the terminal <task>s, the <evaluator_summary>,
    # then the <failure> "evaluator: <summary>" line.
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
    assert "<evaluation_criteria>" not in body
    assert "<failed_criteria>" not in body
    assert body == (
        '<task id="t-a" status="success">\n'
        "generator ok\n"
        "</task>\n"
        "<evaluator_summary>\n"
        "checkout review failed total mismatch\n"
        "</evaluator_summary>\n"
        "<failure>\n"
        "evaluator: checkout review failed total mismatch\n"
        "</failure>"
    )


def test_prior_body_omits_passed_criteria_element():
    # Removed behavior: the <passed_criteria> element no longer appears in the
    # failed-attempt body; the evaluator's text surfaces only as the
    # <evaluator_summary> + <failure> "evaluator: <summary>" line.
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
    assert "<passed_criteria>" not in body
    assert body == (
        '<task id="t-a" status="success">\n'
        "generator ok\n"
        "</task>\n"
        "<evaluator_summary>\n"
        "passing summary\n"
        "</evaluator_summary>\n"
        "<failure>\n"
        "evaluator: passing summary\n"
        "</failure>"
    )


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
        'attempt_no="1"',
        'attempt_no="2"',
        'attempt_no="3"',
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
    # done -> "success" under the presentation-status vocabulary.
    assert [b.metadata["attrs"] for b in task_blocks] == [
        'id="t-a" status="success"',
        'id="t-b" status="success"',
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

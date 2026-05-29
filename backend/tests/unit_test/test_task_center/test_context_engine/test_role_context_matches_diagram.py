"""Diagram-fidelity tests for the role-scoped context redesign.

Renders the planner / generator / evaluator / handoff contexts from
production-shape task ids (``<attempt>:gen:<local_id>``, which exercise the
local-id derivation the diagrams hinge on) and asserts the rendered
``<context>`` body byte-for-byte against the §1 diagrams of
``docs/plans/planner_prior_iteration_context_IMPL_PLAN.md``.

This is the executable form of "verify the resulting context matches the
diagram": each expected string below is transcribed from that plan.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime

from task_center._core.generator_summaries import TaskOutcome, to_record
from task_center.agent_launch.composer import _wrap_context
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.recipes._task_xml import render_task_element
from task_center.context_engine.recipes.attempts import (
    current_attempt_flat_blocks,
    failed_attempt_blocks,
)
from task_center.context_engine.recipes.generator import _dependency_blocks
from task_center.context_engine.recipes.iterations import goal_iteration_blocks
from task_center.context_engine.renderer import XmlPromptRenderer
from task_center.workflow.state import Workflow, WorkflowOriginKind, WorkflowStatus
from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)

_NOW = datetime(2026, 5, 29, tzinfo=UTC)


class _FakeTaskStore:
    def __init__(self, rows: dict[str, dict]) -> None:
        self._rows = rows

    def get_task(self, task_id: str):
        return self._rows.get(task_id)


def _goal() -> Workflow:
    return Workflow(
        id="g1",
        task_center_run_id="run1",
        goal="Build a CLI todo app.",
        status=WorkflowStatus.OPEN,
        iteration_ids=(),
        final_outcome=None,
        created_at=_NOW,
        updated_at=_NOW,
        closed_at=None,
        origin_kind=WorkflowOriginKind.ENTRY,
    )


def _iteration(
    seq: int, status: IterationStatus, goal_text: str, achieved: str | None = None
) -> Iteration:
    return Iteration(
        id=f"it{seq}",
        workflow_id="g1",
        sequence_no=seq,
        creation_reason=IterationCreationReason.INITIAL,
        goal=goal_text,
        attempt_budget=2,
        status=status,
        attempt_ids=(),
        deferred_goal_for_next_iteration=None,
        created_at=_NOW,
        updated_at=_NOW,
        closed_at=_NOW if status is not IterationStatus.OPEN else None,
        plan_spec="prior spec" if achieved is not None else None,
        task_summary=achieved,
    )


def _attempt(
    *,
    status: AttemptStatus,
    fail_reason: AttemptFailReason | None = None,
    generator_task_ids: tuple[str, ...] = (),
    evaluator_task_id: str | None = None,
    evaluation_criteria: tuple[str, ...] = (),
    plan_spec: str = "Full DAG plan.",
) -> Attempt:
    return Attempt(
        id="att1",
        iteration_id="it2",
        attempt_sequence_no=1,
        stage=AttemptStage.CLOSED,
        status=status,
        planner_task_id="att1:planner",
        plan_spec=plan_spec,
        evaluation_criteria=evaluation_criteria,
        generator_task_ids=generator_task_ids,
        evaluator_task_id=evaluator_task_id,
        deferred_goal_for_next_iteration=None,
        fail_reason=fail_reason,
        created_at=_NOW,
        updated_at=_NOW,
        closed_at=_NOW,
    )


def _render(blocks: list[ContextBlock], *, role: str) -> str:
    packet = ContextPacket(
        target_role=role,
        target_id="att1",
        canonical_refs=ContextRefs(workflow_id="g1", iteration_id="it2", attempt_id="att1"),
        blocks=blocks,
        source_ids=[],
    )
    return _wrap_context(XmlPromptRenderer().render_context(packet))


_PRIOR_ACHIEVED = json.dumps(
    [
        to_record(TaskOutcome(local_id="storage", status="success", summary="Implemented storage layer.")),
        to_record(TaskOutcome(local_id="cli_add", status="success", summary="Added the add command.")),
    ]
)


# ---------------------------------------------------------------------------
# Planner — prior iteration + current iteration with one failed attempt.
# ---------------------------------------------------------------------------


def test_planner_context_matches_diagram():
    """§1 Planner: <goal> + <iteration position="prior"> of <task> + current
    <iteration> whose <iteration_goal> precedes a <attempt attempt_no="1"> body
    of <task>s + <failure>. GENERATOR_FAILED is the consistent instance for the
    visible <task> statuses + the ``generator <local_id>:`` failure line."""
    it1 = _iteration(1, IterationStatus.SUCCEEDED, "iteration 1 goal", _PRIOR_ACHIEVED)
    it2 = _iteration(2, IterationStatus.OPEN, "Add list and done commands.")
    failed = _attempt(
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
        generator_task_ids=("att1:gen:cli_list", "att1:gen:cli_done"),
    )
    store = _FakeTaskStore(
        {
            "att1:gen:cli_list": {"status": "done", "summaries": [{"summary": "Implemented list command."}]},
            "att1:gen:cli_done": {
                "status": "failed",
                "summaries": [{"summary": "done command crashed on empty store."}],
            },
        }
    )
    blocks = goal_iteration_blocks(goal=_goal(), current_iteration=it2, iterations=[it1, it2])
    blocks += failed_attempt_blocks(
        current_attempt_id="att2", iteration=it2, attempts=[failed], task_store=store
    )

    expected = (
        "<context>\n"
        "<goal>\n"
        "Build a CLI todo app.\n"
        "</goal>\n"
        "\n"
        '<iteration iteration_no="1" position="prior">\n'
        '<task id="storage" status="success">\n'
        "Implemented storage layer.\n"
        "</task>\n"
        '<task id="cli_add" status="success">\n'
        "Added the add command.\n"
        "</task>\n"
        "</iteration>\n"
        "\n"
        '<iteration iteration_no="2" position="current">\n'
        "<iteration_goal>\n"
        "Add list and done commands.\n"
        "</iteration_goal>\n"
        '<attempt attempt_no="1">\n'
        '<task id="cli_list" status="success">\n'
        "Implemented list command.\n"
        "</task>\n"
        '<task id="cli_done" status="failure">\n'
        "done command crashed on empty store.\n"
        "</task>\n"
        "<failure>\n"
        "generator cli_done: done command crashed on empty store.\n"
        "</failure>\n"
        "</attempt>\n"
        "</iteration>\n"
        "</context>\n"
    )
    assert _render(blocks, role="planner") == expected


def test_planner_failed_attempt_includes_evaluator_summary_when_evaluator_ran():
    """The conditional ``<evaluator_summary>`` slot from the §1 diagram: an
    EVALUATOR_FAILED attempt ran the evaluator, so its summary is rendered
    between the <task>s and the <failure> line."""
    it2 = _iteration(2, IterationStatus.OPEN, "Add list and done commands.")
    failed = _attempt(
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
        generator_task_ids=("att1:gen:cli_list", "att1:gen:cli_done"),
        evaluator_task_id="att1:evaluator",
    )
    store = _FakeTaskStore(
        {
            "att1:gen:cli_list": {"status": "done", "summaries": [{"summary": "Implemented list command."}]},
            "att1:gen:cli_done": {"status": "done", "summaries": [{"summary": "Implemented done command."}]},
            "att1:evaluator": {
                "summaries": [{"summary": "The done command does not persist completion."}]
            },
        }
    )
    body = failed_attempt_blocks(
        current_attempt_id="att2", iteration=it2, attempts=[failed], task_store=store
    )[0].text
    assert body == (
        '<task id="cli_list" status="success">\n'
        "Implemented list command.\n"
        "</task>\n"
        '<task id="cli_done" status="success">\n'
        "Implemented done command.\n"
        "</task>\n"
        "<evaluator_summary>\n"
        "The done command does not persist completion.\n"
        "</evaluator_summary>\n"
        "<failure>\n"
        "evaluator: The done command does not persist completion.\n"
        "</failure>"
    )


# ---------------------------------------------------------------------------
# Generator — plan_spec + <dependency> wrapper of <task> + assigned_task.
# ---------------------------------------------------------------------------


def test_generator_context_matches_diagram():
    store = _FakeTaskStore(
        {
            "att1:gen:storage": {"status": "done", "summaries": [{"summary": "Implemented storage layer."}]},
            "att1:gen:cli_add": {"status": "done", "summaries": [{"summary": "Added the add command."}]},
        }
    )
    blocks: list[ContextBlock] = [
        ContextBlock(
            kind=ContextBlockKind.TASK_SPECIFICATION,
            priority=ContextPriority.HIGH,
            text="Full attempt plan / DAG.",
            source_id="att1",
            source_kind="attempt",
            metadata={"tag": "plan_spec"},
        )
    ]
    blocks += _dependency_blocks(
        needs=("att1:gen:storage", "att1:gen:cli_add"), task_store=store
    )
    blocks.append(
        ContextBlock(
            kind=ContextBlockKind.PLANNED_TASK_SPEC,
            priority=ContextPriority.REQUIRED,
            text="Implement the done command.",
            source_id="att1:gen:cli_done",
            source_kind="task_center_task",
            metadata={"tag": "assigned_task", "attrs": 'task_id="cli_done"'},
        )
    )
    expected = (
        "<context>\n"
        "<plan_spec>\n"
        "Full attempt plan / DAG.\n"
        "</plan_spec>\n"
        "\n"
        "<dependency>\n"
        '<task id="storage" status="success">\n'
        "Implemented storage layer.\n"
        "</task>\n"
        '<task id="cli_add" status="success">\n'
        "Added the add command.\n"
        "</task>\n"
        "</dependency>\n"
        "\n"
        '<assigned_task task_id="cli_done">\n'
        "Implement the done command.\n"
        "</assigned_task>\n"
        "</context>\n"
    )
    assert _render(blocks, role="generator") == expected


# ---------------------------------------------------------------------------
# Evaluator — plan_spec + flat <task>×N + evaluation_criteria.
# ---------------------------------------------------------------------------


def test_evaluator_context_matches_diagram():
    attempt = _attempt(
        status=AttemptStatus.RUNNING,
        generator_task_ids=(
            "att1:gen:storage",
            "att1:gen:cli_add",
            "att1:gen:cli_list",
            "att1:gen:cli_done",
        ),
        evaluation_criteria=("the add command works", "listing works", "done marks complete"),
    )
    store = _FakeTaskStore(
        {
            "att1:gen:storage": {"status": "done", "summaries": [{"summary": "Implemented storage layer."}]},
            "att1:gen:cli_add": {"status": "done", "summaries": [{"summary": "Added the add command."}]},
            "att1:gen:cli_list": {"status": "done", "summaries": [{"summary": "Added the list command."}]},
            "att1:gen:cli_done": {"status": "done", "summaries": [{"summary": "Added the done command."}]},
        }
    )
    blocks = current_attempt_flat_blocks(attempt=attempt, task_store=store)
    expected = (
        "<context>\n"
        "<plan_spec>\n"
        "Full DAG plan.\n"
        "</plan_spec>\n"
        "\n"
        '<task id="storage" status="success">\n'
        "Implemented storage layer.\n"
        "</task>\n"
        "\n"
        '<task id="cli_add" status="success">\n'
        "Added the add command.\n"
        "</task>\n"
        "\n"
        '<task id="cli_list" status="success">\n'
        "Added the list command.\n"
        "</task>\n"
        "\n"
        '<task id="cli_done" status="success">\n'
        "Added the done command.\n"
        "</task>\n"
        "\n"
        "<evaluation_criteria>\n"
        "the add command works\n"
        "listing works\n"
        "done marks complete\n"
        "</evaluation_criteria>\n"
        "</context>\n"
    )
    assert _render(blocks, role="evaluator") == expected


# ---------------------------------------------------------------------------
# Handoff — nested <task> roll-up (success + failure).
# ---------------------------------------------------------------------------


def test_handoff_success_nested_task_matches_diagram():
    parent = TaskOutcome(
        local_id="implement_auth",
        status="success",
        summary=None,
        children=(
            TaskOutcome(local_id="schema", status="success", summary="Designed the schema."),
            TaskOutcome(local_id="login_api", status="success", summary="Built the login API."),
            TaskOutcome(local_id="session_mw", status="success", summary="Added session middleware."),
        ),
    )
    assert render_task_element(parent) == (
        '<task id="implement_auth" status="success">\n'
        '<task id="schema" status="success">\n'
        "Designed the schema.\n"
        "</task>\n"
        '<task id="login_api" status="success">\n'
        "Built the login API.\n"
        "</task>\n"
        '<task id="session_mw" status="success">\n'
        "Added session middleware.\n"
        "</task>\n"
        "</task>"
    )


def test_handoff_failure_nested_task_matches_diagram():
    parent = TaskOutcome(
        local_id="implement_auth",
        status="failure",
        summary=None,
        children=(
            TaskOutcome(local_id="schema", status="success", summary="Designed the schema."),
            TaskOutcome(local_id="login_api", status="failure", summary="Login API failed on token refresh."),
        ),
        failure="generator login_api: token refresh raised.",
    )
    assert render_task_element(parent) == (
        '<task id="implement_auth" status="failure">\n'
        '<task id="schema" status="success">\n'
        "Designed the schema.\n"
        "</task>\n"
        '<task id="login_api" status="failure">\n'
        "Login API failed on token refresh.\n"
        "</task>\n"
        "<failure>\n"
        "generator login_api: token refresh raised.\n"
        "</failure>\n"
        "</task>"
    )


def test_handoff_rollup_renders_through_evaluator_task_block():
    """A parent generator carrying a ``handoff_rollup`` payload renders its
    nested ``<task>`` roll-up wherever the generator appears — here as one of
    the evaluator's flat task blocks (unit-level stand-in for the end-to-end
    handoff, per the design note)."""
    rollup = {
        "children": [
            to_record(TaskOutcome(local_id="schema", status="success", summary="Designed the schema.")),
            to_record(TaskOutcome(local_id="login_api", status="success", summary="Built the login API.")),
        ],
        "failure": None,
    }
    attempt = _attempt(
        status=AttemptStatus.RUNNING,
        generator_task_ids=("att1:gen:implement_auth",),
        evaluation_criteria=("auth works",),
    )
    store = _FakeTaskStore(
        {
            "att1:gen:implement_auth": {
                "status": "done",
                "summaries": [
                    {
                        "outcome": "success",
                        "summary": "Delegated goal succeeded.",
                        "payload": {"handoff_rollup": rollup},
                    }
                ],
            }
        }
    )
    rendered = XmlPromptRenderer().render_context(
        ContextPacket(
            target_role="evaluator",
            target_id="att1",
            canonical_refs=ContextRefs(workflow_id="g1", iteration_id="it2", attempt_id="att1"),
            blocks=current_attempt_flat_blocks(attempt=attempt, task_store=store),
            source_ids=[],
        )
    )
    assert (
        '<task id="implement_auth" status="success">\n'
        '<task id="schema" status="success">\n'
        "Designed the schema.\n"
        "</task>\n"
        '<task id="login_api" status="success">\n'
        "Built the login API.\n"
        "</task>\n"
        "</task>"
    ) in rendered

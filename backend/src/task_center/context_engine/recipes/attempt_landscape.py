"""Failed attempt landscape blocks for planner context."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, TYPE_CHECKING

from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPriority,
)
from task_center.context_engine.recipes._summaries import latest_summary_text
from task_center.attempt.state import Attempt, AttemptStatus

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from db.stores.task_center_store import TaskCenterStore


@dataclass(frozen=True, slots=True)
class _GeneratorOutcome:
    task_id: str
    status: str
    blocked_by: str | None
    summary: str | None


def failed_attempt_landscape_blocks(
    *,
    current_attempt_id: str | None,
    attempts: list[Attempt],
    task_store: TaskCenterStore | None = None,
) -> list[ContextBlock]:
    failed = sorted(
        (
            g
            for g in attempts
            if g.status == AttemptStatus.FAILED
            and g.id != current_attempt_id
        ),
        key=lambda g: g.attempt_sequence_no,
    )
    if not failed:
        return []

    return [
        ContextBlock(
            kind=ContextBlockKind.FAILED_ATTEMPT_LANDSCAPE,
            priority=ContextPriority.HIGH,
            text=_render_failed_attempt(g, task_store=task_store),
            source_id=g.id,
            source_kind="attempt",
            metadata={
                "attempt_sequence_no": str(g.attempt_sequence_no),
                "group_heading": "# Prior Failed Attempts",
                "subheading": f"Attempt {g.attempt_sequence_no}",
            },
        )
        for g in failed
    ]


def _render_failed_attempt(
    attempt: Attempt, *, task_store: TaskCenterStore | None
) -> str:
    outcomes = _generator_outcomes(attempt, task_store=task_store)
    sections = [
        _render_accepted_plan(attempt),
        _render_generator_outcomes(outcomes),
    ]
    evaluator = _render_evaluator_judgment(
        attempt, outcomes=outcomes, task_store=task_store
    )
    if evaluator:
        sections.append(evaluator)
    return "\n\n".join(sections)


def _render_accepted_plan(attempt: Attempt) -> str:
    specification = attempt.task_specification or "(not submitted)"
    return (
        "### Accepted Plan\n\n"
        f"Plan type: {_plan_kind(attempt)}\n\n"
        f"Specification:\n{specification}"
    )


def _render_generator_outcomes(outcomes: list[_GeneratorOutcome]) -> str:
    status_lines = _status_summary_lines(outcomes)
    detail_sections = [
        _render_generator_detail(outcome)
        for outcome in outcomes
        if _should_render_generator_detail(outcome)
    ]
    body = "### Generator Outcomes\n\nStatus summary:\n" + "\n".join(
        status_lines
    )
    if detail_sections:
        body += "\n\n" + "\n\n".join(detail_sections)
    return body


def _status_summary_lines(outcomes: list[_GeneratorOutcome]) -> list[str]:
    if not outcomes:
        return ["- (no generator tasks recorded)"]
    lines: list[str] = []
    for outcome in outcomes:
        if outcome.blocked_by:
            lines.append(
                f"- {outcome.task_id}: {outcome.status} by {outcome.blocked_by}"
            )
        else:
            lines.append(f"- {outcome.task_id}: {outcome.status}")
    return lines


def _render_generator_detail(outcome: _GeneratorOutcome) -> str:
    return f"#### {outcome.task_id}\n\n{outcome.summary}"


def _should_render_generator_detail(outcome: _GeneratorOutcome) -> bool:
    if outcome.summary is None:
        return False
    if outcome.summary in {
        "(empty)",
        "(no summary recorded)",
    }:
        return False
    return True


def _render_evaluator_judgment(
    attempt: Attempt,
    *,
    outcomes: list[_GeneratorOutcome],
    task_store: TaskCenterStore | None,
) -> str:
    if _has_premature_generator_failure(outcomes):
        return ""
    if task_store is None or attempt.evaluator_task_id is None:
        return ""

    task = task_store.get_task(attempt.evaluator_task_id)
    if task is None:
        evaluator_summary = "(missing evaluator task row)"
    else:
        evaluator_summary = latest_summary_text(task.get("summaries"))

    criteria_block = (
        "\n".join(f"  - {c}" for c in attempt.evaluation_criteria) or "  (none)"
    )
    return (
        "### Evaluator Judgment\n\n"
        f"Evaluation criteria:\n{criteria_block}\n\n"
        f"Evaluator summary:\n{evaluator_summary}"
    )


def _has_premature_generator_failure(outcomes: list[_GeneratorOutcome]) -> bool:
    return any(
        outcome.status in {"failed", "blocked", "missing task row"}
        for outcome in outcomes
    )


def _generator_outcomes(
    attempt: Attempt, *, task_store: TaskCenterStore | None
) -> list[_GeneratorOutcome]:
    if task_store is None or not attempt.generator_task_ids:
        return []

    outcomes: list[_GeneratorOutcome] = []
    for task_id in attempt.generator_task_ids:
        task = task_store.get_task(task_id)
        if task is None:
            outcomes.append(
                _GeneratorOutcome(
                    task_id=task_id,
                    status="missing task row",
                    blocked_by=None,
                    summary=None,
                )
            )
            continue
        summaries = task.get("summaries")
        outcomes.append(
            _GeneratorOutcome(
                task_id=task_id,
                status=str(task.get("status") or "unknown"),
                blocked_by=_blocked_by(summaries),
                summary=latest_summary_text(summaries).strip(),
            )
        )
    return outcomes


def _blocked_by(summaries: list[Any] | None) -> str | None:
    if not summaries:
        return None
    latest = summaries[-1]
    if not isinstance(latest, dict):
        return None
    blocked_by = latest.get("blocked_by")
    return str(blocked_by) if blocked_by else None


def _plan_kind(attempt: Attempt) -> str:
    if attempt.continuation_goal:
        return "partial"
    if (
        attempt.task_specification
        or attempt.evaluation_criteria
        or attempt.generator_task_ids
    ):
        return "full"
    return "unsubmitted"

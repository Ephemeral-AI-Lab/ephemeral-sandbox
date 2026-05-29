"""Shared, presentation-free helpers for projecting generator task outcomes.

This module is the single source of truth for the *data* behind a
``<task id="<local_id>" status="<success|failure|pending>">`` element: the
projection off a task row's ``summaries`` list, the internal-enum →
presentation-status mapping, the local-id derivation, the per-generator
outcome record, the failure line, and the JSON round-trip for the
denormalized achieved-record / handoff roll-up.

It deliberately holds **no XML and no ``ContextEngineError``** — rendering and
hostile-body sanitization live in the ``context_engine`` layer
(``recipes/_task_xml.py``), which depends on this module, never the reverse.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from task_center.attempt.state import Attempt, AttemptFailReason
from task_center.iteration.state import IterationStatus
from task_center._core.task_state import TERMINAL_GENERATOR_STATUSES

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from task_center._core.persistence import IterationStoreProtocol, TaskStoreProtocol

_NO_SUMMARY = "(no summary recorded)"
_EMPTY = "(empty)"
_NO_DETAIL = "(no detail recorded)"
EMPTY_SUMMARY_PLACEHOLDERS: frozenset[str] = frozenset({_EMPTY, _NO_SUMMARY})

_GEN_SEP = ":gen:"
_RUN_EXHAUSTED = "run_exhausted"

# Internal enum value → presentation status. Unknown values (``running``,
# ``waiting_workflow``, ``"missing task row"``) fall through unchanged so callers
# stay presence-defensive.
_PRESENTATION: dict[str, str] = {
    "done": "success",
    "failed": "failure",
    "blocked": "failure",
    "pending": "pending",
}
_TERMINAL_RAW: frozenset[str] = frozenset(s.value for s in TERMINAL_GENERATOR_STATUSES)
_MISSING_TASK_ROW_STATUS = "missing task row"


@dataclass(frozen=True, slots=True)
class TaskOutcome:
    """One generator/task outcome, the data behind a ``<task>`` element.

    ``status`` is the presentation status; ``raw_status`` is the internal task
    status (``None`` when rebuilt from a serialized record, where the raw value
    is no longer needed). ``children`` carries a handoff roll-up (one or more
    levels of nested ``<task>``); ``failure`` is the ``<failure>`` line for a
    failed handoff.
    """

    local_id: str
    status: str
    summary: str | None
    children: tuple["TaskOutcome", ...] = ()
    failure: str | None = None
    raw_status: str | None = None

    @property
    def is_terminal(self) -> bool:
        return self.raw_status in _TERMINAL_RAW


def latest_task_summary(summaries: list[Any] | None) -> str:
    """Return the most recent summary string from a task's ``summaries`` list.

    Prefers ``summary`` then ``outcome``; falls back to placeholders. Moved
    verbatim from the deleted ``recipes/summaries.py`` so the three former
    callers keep identical behavior.
    """
    if not summaries:
        return _NO_SUMMARY
    last = summaries[-1]
    if not isinstance(last, dict):
        return str(last)
    return str(last.get("summary") or last.get("outcome") or _EMPTY)


def present_status(raw_status: str) -> str:
    """Map an internal task status to the presentation vocabulary.

    ``done→success``, ``failed|blocked→failure``, ``pending→pending``. Any
    other value (``running``/``waiting_workflow``/``missing task row``) passes
    through unchanged.
    """
    return _PRESENTATION.get(raw_status, raw_status)


def local_id_of(task_id: str) -> str:
    """Derive the planner-assigned local id from a generator task id.

    Generator task ids are ``"<attempt_id>:gen:<local_id>"``. Short fixture
    ids without the separator pass through unchanged.
    """
    return task_id.split(_GEN_SEP, 1)[1] if _GEN_SEP in task_id else task_id


def task_outcome_from_row(task_id: str, task: dict[str, Any] | None) -> TaskOutcome:
    """Build a :class:`TaskOutcome` from a (possibly missing) task row."""
    local_id = local_id_of(task_id)
    if task is None:
        return TaskOutcome(
            local_id=local_id, status=_MISSING_TASK_ROW_STATUS, summary=None, raw_status=None
        )
    raw_status = str(task.get("status") or "unknown")
    summaries = task.get("summaries")
    children, failure = _handoff_rollup(summaries)
    return TaskOutcome(
        local_id=local_id,
        status=present_status(raw_status),
        summary=latest_task_summary(summaries),
        children=children,
        failure=failure,
        raw_status=raw_status,
    )


def generator_outcomes(
    attempt: Attempt, *, task_store: TaskStoreProtocol | None
) -> list[TaskOutcome]:
    """Return one :class:`TaskOutcome` per generator task, in DAG order."""
    if task_store is None or not attempt.generator_task_ids:
        return []
    return [
        task_outcome_from_row(task_id, task_store.get_task(task_id))
        for task_id in attempt.generator_task_ids
    ]


def attempt_failure_line(attempt: Attempt, task_store: TaskStoreProtocol | None) -> str:
    """Render the ``<failure>`` body for *attempt* from its ``fail_reason``.

    See IMPL_PLAN §2.4: ``planner: <summary>`` / ``generator <local_id>:
    <summary>`` (one line per failed/blocked generator) / ``evaluator:
    <summary>`` / ``agent_launch_failed``. Appends ``(terminated)`` when the
    failing task's latest ``payload.fail_reason == "run_exhausted"``.
    Presence-defensive: ``(no detail recorded)`` when nothing is available.
    """
    reason = attempt.fail_reason
    if reason == AttemptFailReason.STARTUP_FAILED:
        return "agent_launch_failed"
    if reason == AttemptFailReason.PLANNER_FAILED:
        return _stage_failure_line("planner", attempt.planner_task_id, task_store)
    if reason == AttemptFailReason.EVALUATOR_FAILED:
        return _stage_failure_line("evaluator", attempt.evaluator_task_id, task_store)
    if reason == AttemptFailReason.GENERATOR_FAILED:
        return _generator_failure_lines(attempt, task_store)
    return _NO_DETAIL


# ---- handoff roll-up / achieved-record JSON round-trip --------------------


def to_record(outcome: TaskOutcome) -> dict[str, Any]:
    """Serialize a :class:`TaskOutcome` to a JSON-safe dict (drops raw_status)."""
    record: dict[str, Any] = {
        "local_id": outcome.local_id,
        "status": outcome.status,
        "summary": outcome.summary,
    }
    if outcome.children:
        record["children"] = [to_record(child) for child in outcome.children]
    if outcome.failure is not None:
        record["failure"] = outcome.failure
    return record


def from_record(record: dict[str, Any]) -> TaskOutcome:
    """Rebuild a :class:`TaskOutcome` from a serialized dict."""
    summary = record.get("summary")
    failure = record.get("failure")
    return TaskOutcome(
        local_id=str(record.get("local_id") or ""),
        status=str(record.get("status") or "pending"),
        summary=None if summary is None else str(summary),
        children=tuple(
            from_record(child)
            for child in record.get("children") or ()
            if isinstance(child, dict)
        ),
        failure=None if failure is None else str(failure),
    )


def parse_achieved_record(task_summary: str | None) -> list[TaskOutcome]:
    """Parse a denormalized iteration achieved-record into outcomes.

    Degrades gracefully for legacy (pre-migration) rows whose ``task_summary``
    is free text rather than a JSON list: such a row renders as a single
    ``<task>`` carrying the legacy text.
    """
    if not task_summary:
        return []
    try:
        data = json.loads(task_summary)
    except (ValueError, TypeError):
        data = None
    if not isinstance(data, list):
        return [TaskOutcome(local_id="summary", status="success", summary=str(task_summary))]
    return [from_record(item) for item in data if isinstance(item, dict)]


def child_outcomes_for_workflow(
    workflow_id: str, iteration_store: IterationStoreProtocol
) -> list[TaskOutcome]:
    """Flatten the achieved records of a goal's SUCCEEDED iterations, in order."""
    outcomes: list[TaskOutcome] = []
    for iteration in iteration_store.list_for_workflow(workflow_id):
        if iteration.status != IterationStatus.SUCCEEDED:
            continue
        outcomes.extend(parse_achieved_record(iteration.task_summary))
    return outcomes


# ---- internals ------------------------------------------------------------


def _handoff_rollup(
    summaries: list[Any] | None,
) -> tuple[tuple[TaskOutcome, ...], str | None]:
    """Read a ``payload.handoff_rollup`` off the latest summary, if present."""
    if not summaries:
        return (), None
    latest = summaries[-1]
    if not isinstance(latest, dict):
        return (), None
    payload = latest.get("payload")
    if not isinstance(payload, dict):
        return (), None
    rollup = payload.get("handoff_rollup")
    if not isinstance(rollup, dict):
        return (), None
    children = tuple(
        from_record(child) for child in rollup.get("children") or () if isinstance(child, dict)
    )
    failure = rollup.get("failure")
    return children, (failure if isinstance(failure, str) else None)


def _is_terminated(task: dict[str, Any]) -> bool:
    summaries = task.get("summaries")
    if not summaries:
        return False
    latest = summaries[-1]
    if not isinstance(latest, dict):
        return False
    payload = latest.get("payload")
    if not isinstance(payload, dict):
        return False
    return payload.get("fail_reason") == _RUN_EXHAUSTED


def _stage_failure_line(
    role: str, task_id: str | None, task_store: TaskStoreProtocol | None
) -> str:
    if task_store is None or task_id is None:
        return f"{role}: {_NO_DETAIL}"
    task = task_store.get_task(task_id)
    if task is None:
        return f"{role}: {_NO_DETAIL}"
    suffix = " (terminated)" if _is_terminated(task) else ""
    return f"{role}: {latest_task_summary(task.get('summaries'))}{suffix}"


def _generator_failure_lines(attempt: Attempt, task_store: TaskStoreProtocol | None) -> str:
    if task_store is None:
        return f"generator: {_NO_DETAIL}"
    lines: list[str] = []
    for task_id in attempt.generator_task_ids:
        task = task_store.get_task(task_id)
        if task is None:
            continue
        if str(task.get("status") or "") not in ("failed", "blocked"):
            continue
        suffix = " (terminated)" if _is_terminated(task) else ""
        lines.append(
            f"generator {local_id_of(task_id)}: "
            f"{latest_task_summary(task.get('summaries'))}{suffix}"
        )
    return "\n".join(lines) if lines else f"generator: {_NO_DETAIL}"


__all__ = [
    "EMPTY_SUMMARY_PLACEHOLDERS",
    "TaskOutcome",
    "attempt_failure_line",
    "child_outcomes_for_workflow",
    "from_record",
    "generator_outcomes",
    "latest_task_summary",
    "local_id_of",
    "parse_achieved_record",
    "present_status",
    "task_outcome_from_row",
    "to_record",
]

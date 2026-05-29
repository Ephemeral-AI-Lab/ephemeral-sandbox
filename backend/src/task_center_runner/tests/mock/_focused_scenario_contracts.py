"""Shared assertions for focused mocked-agent integration scenarios."""

from __future__ import annotations

from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field

from task_center_runner.audit.events import EventType
from task_center_runner.core.runner import RunReport
from task_center_runner.scenarios.base import Scenario


@dataclass(frozen=True, slots=True)
class FocusedScenarioCase:
    name: str
    expected_status: str = "done"
    min_event_counts: Mapping[EventType, int] = field(default_factory=dict)
    absent_events: Sequence[EventType] = ()
    workflow_status: str = "succeeded"
    iteration_count: int | None = 1
    attempt_count: int | None = None


def assert_focused_scenario_report(
    report: RunReport,
    scenario: Scenario,
    case: FocusedScenarioCase,
) -> None:
    assert report.task_center_status == case.expected_status, report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]
    assert (report.run_dir / "run.json").exists()
    assert (report.run_dir / "metrics.json").exists()
    _assert_ordered_subsequence(
        scenario.expected_event_sequence,
        report.seen_event_types,
    )
    _assert_event_counts(report, case)
    _assert_graph_shape(report, case)


def count_role_tasks(
    report: RunReport,
    role: str,
    *,
    status: str | None = None,
) -> int:
    """Count generator tasks of *role* across all attempts in ``graph_summary``.

    The §4.1 replacement for ``count_events(<ROLE>_INVOKED/<ROLE>_SUCCESS)``:
    lifecycle events are gone, so workflow fan-out is asserted via real store
    state. ``status="done"`` counts only succeeded tasks (the ``EXECUTOR_SUCCESS``
    analog); ``status=None`` counts every task of that role (the ``_INVOKED``
    analog).
    """
    total = 0
    for goal in report.graph_summary["workflows"]:
        for iteration in goal["iterations"]:
            for attempt in iteration["attempts"]:
                for task in attempt["tasks"]:
                    if str(task.get("agent_name") or "") != role:
                        continue
                    if status is not None and str(task.get("status") or "") != status:
                        continue
                    total += 1
    return total


def recursive_goals(graph_summary: Mapping[str, object]) -> list[dict]:
    """Return the delegated (recursive) workflows from ``graph_summary``.

    A recursive workflow is one started by an executor ``submit_execution_handoff``
    (``origin_kind == "task"``), as opposed to the entry workflow. The §4.1
    replacement for ``count_events(RECURSIVE_WORKFLOW_REQUESTED/COMPLETED)``:
    lifecycle events are gone under the event-source runner, so recursion is
    asserted via real store state.
    """
    workflows = graph_summary["workflows"]  # type: ignore[index]
    return [
        workflow
        for workflow in workflows  # type: ignore[union-attr]
        if str(workflow.get("origin_kind") or "") == "task"
    ]


def _assert_ordered_subsequence(
    expected: Sequence[EventType],
    actual: Sequence[EventType],
) -> None:
    position = 0
    for event_type in actual:
        if position < len(expected) and event_type == expected[position]:
            position += 1
    assert position == len(expected), (
        "expected_event_sequence was not observed in order: "
        f"expected={[event.value for event in expected]} "
        f"actual={[event.value for event in actual]}"
    )


def _assert_event_counts(report: RunReport, case: FocusedScenarioCase) -> None:
    counts = Counter(event.type for event in report.events)
    for event_type, minimum in case.min_event_counts.items():
        assert counts[event_type] >= minimum, (
            f"{case.name}: expected at least {minimum} {event_type.value} events, "
            f"saw {counts[event_type]}"
        )
    for event_type in case.absent_events:
        assert counts[event_type] == 0, (
            f"{case.name}: did not expect {event_type.value}, saw "
            f"{counts[event_type]}"
        )


def _assert_graph_shape(report: RunReport, case: FocusedScenarioCase) -> None:
    goals = report.graph_summary["workflows"]
    assert len(goals) == 1, report.graph_summary
    goal = goals[0]
    assert goal["status"] == case.workflow_status
    if case.iteration_count is not None:
        assert len(goal["iterations"]) == case.iteration_count
    if case.attempt_count is not None:
        attempts = [
            attempt
            for iteration in goal["iterations"]
            for attempt in iteration["attempts"]
        ]
        assert len(attempts) == case.attempt_count

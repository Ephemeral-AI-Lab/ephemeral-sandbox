"""Shared assertions for focused mocked-agent integration scenarios."""

from __future__ import annotations

from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field

from task_center_runner.audit.events import EventType
from task_center_runner.core.runner import RunReport


@dataclass(frozen=True, slots=True)
class FocusedScenarioCase:
    name: str
    expected_status: str = "done"
    min_event_counts: Mapping[EventType, int] = field(default_factory=dict)
    absent_events: Sequence[EventType] = ()
    min_role_tasks: Mapping[str, int] = field(default_factory=dict)
    min_done_role_tasks: Mapping[str, int] = field(default_factory=dict)
    min_failed_role_tasks: Mapping[str, int] = field(default_factory=dict)
    absent_role_tasks: Sequence[str] = ()
    absent_done_role_tasks: Sequence[str] = ()
    min_deferred_attempts: int = 0
    max_deferred_attempts: int | None = None
    workflow_status: str = "succeeded"
    iteration_count: int | None = 1
    attempt_count: int | None = None


def assert_focused_scenario_report(
    report: RunReport,
    case: FocusedScenarioCase,
) -> None:
    assert report.task_center_status == case.expected_status, report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    _assert_dependency_prompt_inspections(report)
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]
    assert (report.run_dir / "run.json").exists()
    assert (report.run_dir / "metrics.json").exists()
    _assert_event_counts(report, case)
    _assert_role_counts(report, case)
    _assert_graph_shape(report, case)


# Both FAILED and BLOCKED are non-success terminal generator statuses in
# TaskCenter (TERMINAL_GENERATOR_STATUSES); either one fails its attempt. A
# ``submit_generator_failure`` task is "a task that failed the attempt" for the
# purposes of the scenario role counts.
_FAILED_STATUSES: tuple[str, ...] = ("failed", "blocked")
_PROMPT_INSPECTED_STATUSES: frozenset[str] = frozenset(
    {"done", "failed", "waiting_workflow"}
)


def count_role_tasks(
    report: RunReport,
    role: str,
    *,
    status: str | tuple[str, ...] | None = None,
) -> int:
    """Count generator tasks of *role* across all attempts in ``graph_summary``.

    Workflow fan-out is asserted via real store state. ``status="done"`` counts
    only succeeded tasks; a tuple counts any task whose status is in the tuple;
    ``status=None`` counts every task of that role.
    """
    allowed = (status,) if isinstance(status, str) else status
    total = 0
    for workflow in report.graph_summary["workflows"]:
        for iteration in workflow["iterations"]:
            for attempt in iteration["attempts"]:
                for task in attempt["tasks"]:
                    if str(task.get("agent_name") or "") != role:
                        continue
                    if allowed is not None and str(task.get("status") or "") not in allowed:
                        continue
                    total += 1
    return total


def recursive_workflows(graph_summary: Mapping[str, object]) -> list[dict]:
    """Return the delegated (recursive) workflows from ``graph_summary``.

    A recursive workflow is one started by an executor ``submit_workflow_handoff``;
    its ``parent_task_id`` points at a generator task rather than the synthetic
    run-level bootstrap task ``<run_id>:root`` that parents the entry workflow.
    """
    workflows = graph_summary["workflows"]  # type: ignore[index]
    return [
        workflow
        for workflow in workflows  # type: ignore[union-attr]
        if not str(workflow.get("parent_task_id") or "").endswith(":root")
    ]


def count_deferred_attempts(report: RunReport) -> int:
    return sum(
        1
        for workflow in report.graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        if attempt["deferred_goal_for_next_iteration"]
    )


def _assert_event_counts(report: RunReport, case: FocusedScenarioCase) -> None:
    counts = Counter(event.type for event in report.events)
    for event_type, minimum in case.min_event_counts.items():
        observed = counts[event_type]
        assert observed >= minimum, (
            f"{case.name}: expected at least {minimum} {event_type.value} events, "
            f"saw {observed}"
        )
    for event_type in case.absent_events:
        assert counts[event_type] == 0, (
            f"{case.name}: did not expect {event_type.value}, saw "
            f"{counts[event_type]}"
        )


def _assert_role_counts(report: RunReport, case: FocusedScenarioCase) -> None:
    for role, minimum in case.min_role_tasks.items():
        observed = count_role_tasks(report, role)
        assert observed >= minimum, (
            f"{case.name}: expected at least {minimum} {role} tasks, saw {observed}"
        )
    for role, minimum in case.min_done_role_tasks.items():
        observed = count_role_tasks(report, role, status="done")
        assert observed >= minimum, (
            f"{case.name}: expected at least {minimum} done {role} tasks, "
            f"saw {observed}"
        )
    for role, minimum in case.min_failed_role_tasks.items():
        observed = count_role_tasks(report, role, status=_FAILED_STATUSES)
        assert observed >= minimum, (
            f"{case.name}: expected at least {minimum} failed {role} tasks, "
            f"saw {observed}"
        )
    for role in case.absent_role_tasks:
        observed = count_role_tasks(report, role)
        assert observed == 0, (
            f"{case.name}: did not expect {role} tasks, saw {observed}"
        )
    for role in case.absent_done_role_tasks:
        observed = count_role_tasks(report, role, status="done")
        assert observed == 0, (
            f"{case.name}: did not expect done {role} tasks, saw {observed}"
        )

    deferred = count_deferred_attempts(report)
    assert deferred >= case.min_deferred_attempts, (
        f"{case.name}: expected at least {case.min_deferred_attempts} deferred "
        f"attempts, saw {deferred}"
    )
    if case.max_deferred_attempts is not None:
        assert deferred <= case.max_deferred_attempts, (
            f"{case.name}: expected at most {case.max_deferred_attempts} "
            f"deferred attempts, saw {deferred}"
        )


def _assert_dependency_prompt_inspections(report: RunReport) -> None:
    inspections_by_task = {
        inspection.task_id: inspection for inspection in report.prompt_inspections
    }
    for task in _graph_tasks(report):
        needs = tuple(str(dep) for dep in task.get("needs") or ())
        if not needs:
            continue
        inspection = inspections_by_task.get(str(task.get("task_id") or ""))
        if inspection is None:
            if str(task.get("status") or "") in _PROMPT_INSPECTED_STATUSES:
                raise AssertionError(
                    f"{task['task_id']}: launched dependency-aware task "
                    "had no prompt inspection"
                )
            continue
        assert inspection.checks.get("dependencies") is True, (
            f"{task['task_id']}: launched dependency-aware task prompt "
            "did not include <dependencies>"
        )
        assert inspection.checks.get("dependency_outcomes") is True, (
            f"{task['task_id']}: launched dependency-aware task prompt "
            "did not include dependency task outcomes"
        )


def _graph_tasks(report: RunReport) -> list[dict[str, object]]:
    return [
        task
        for workflow in report.graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
    ]


def _assert_graph_shape(report: RunReport, case: FocusedScenarioCase) -> None:
    workflows = report.graph_summary["workflows"]
    assert len(workflows) == 1, report.graph_summary
    workflow = workflows[0]
    assert workflow["status"] == case.workflow_status
    if case.iteration_count is not None:
        assert len(workflow["iterations"]) == case.iteration_count
    if case.attempt_count is not None:
        attempts = [
            attempt
            for iteration in workflow["iterations"]
            for attempt in iteration["attempts"]
        ]
        assert len(attempts) == case.attempt_count

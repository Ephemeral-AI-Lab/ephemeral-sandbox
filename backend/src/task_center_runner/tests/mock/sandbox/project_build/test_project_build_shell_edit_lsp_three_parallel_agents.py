"""Diagnostic for three parallel shell/edit/LSP mock agents in one run."""

from __future__ import annotations

from collections import Counter
from collections.abc import Mapping, Sequence
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.audit.events import EventType
from task_center_runner.core.runner import RunReport
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios.base import (
    ScenarioBase,
    ScenarioContext,
    ToolCallSpec,
)
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    load_performance_report,
    mapping,
)
from tools.submission.evaluator import submit_evaluation_success
from tools.submission.planner import submit_plan_closes_goal


pytestmark = pytest.mark.asyncio

_AGENT_TASK_IDS = tuple(
    f"complex_project_build_shell_edit_lsp_agent_{index}"
    for index in range(3)
)


class ComplexProjectBuildShellEditLspThreeParallelAgents(ScenarioBase):
    """Three dependency-free executor tasks inside one TaskCenter run."""

    name = "sandbox.complex_project_build_shell_edit_lsp_three_parallel_agents"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(submit_plan_closes_goal, _three_agent_plan())

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:  # noqa: ARG002
        return ("complex_project_build_shell_edit_lsp_shared_bootstrap",)

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": (
                    "Three parallel mixed shell-edit + LSP project-build "
                    "executors completed in one TaskCenter run."
                ),
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(7200)
async def test_project_build_shell_edit_lsp_three_parallel_agents(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = ComplexProjectBuildShellEditLspThreeParallelAgents()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.task_center_status == "failed", report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    _assert_three_executor_tasks_per_attempt(report)
    _assert_shared_bootstrap_conflicts(report)

    perf = await _load_task_center_performance_report(report)
    per_tool = mapping(mapping(perf["tools"])["per_tool"])
    assert _max_overlapping_tool_calls(per_tool) >= len(_AGENT_TASK_IDS)


def _three_agent_plan() -> dict[str, Any]:
    task_specs = {
        task_id: (
            "Run the full mixed shell-edit + LSP saturation project-build "
            "probe under /ephemeral-os as one of three parallel executor "
            "agents in the same TaskCenter attempt."
        )
        for task_id in _AGENT_TASK_IDS
    }
    return {
        "plan_spec": (
            "Launch three mocked executor agents in parallel inside one "
            "TaskCenter run; each executor runs the full mixed shell-edit + "
            "semantic LSP project-build probe."
        ),
        "evaluation_criteria": [
            "The planner emits exactly three dependency-free executor tasks.",
            "All three executor tasks launch together in each attempt.",
            "The performance report shows overlapping executor tool calls.",
            "Shared bootstrap writes fail fast with typed OCC conflicts.",
        ],
        "tasks": [
            {"id": task_id, "agent_name": "executor", "deps": []}
            for task_id in _AGENT_TASK_IDS
        ],
        "task_specs": task_specs,
    }


def _assert_three_executor_tasks_per_attempt(report: RunReport) -> None:
    executor_launches = [
        launch for launch in report.launches if launch.role == "executor"
    ]
    by_attempt: dict[str | None, list[str]] = {}
    for launch in executor_launches:
        _assert_canonical_generator_task_id(launch.task_id, launch.attempt_id)
        by_attempt.setdefault(launch.attempt_id, []).append(
            _local_generator_task_id(launch.task_id)
        )

    assert len(by_attempt) == 2, by_attempt
    for task_ids in by_attempt.values():
        assert task_ids == list(_AGENT_TASK_IDS)

    goals = report.graph_summary["workflows"]
    assert len(goals) == 1, report.graph_summary
    attempts = goals[0]["iterations"][0]["attempts"]
    assert len(attempts) == 2, report.graph_summary
    generator_status_counts: Counter[str] = Counter()
    for attempt in attempts:
        assert attempt["status"] == "failed"
        assert attempt["fail_reason"] == "generator_failed"
        for task_id in attempt["task_ids"]:
            _assert_canonical_generator_task_id(task_id, attempt["id"])
        assert [
            _local_generator_task_id(task_id) for task_id in attempt["task_ids"]
        ] == list(_AGENT_TASK_IDS)

        generator_tasks = [
            task
            for task in attempt["tasks"]
            if _local_generator_task_id(task["id"]) in _AGENT_TASK_IDS
        ]
        assert len(generator_tasks) == len(_AGENT_TASK_IDS)
        assert all(task["needs"] == [] for task in generator_tasks)
        generator_status_counts.update(str(task["status"]) for task in generator_tasks)

    counts = Counter(event.type for event in report.events)
    assert counts[EventType.EXECUTOR_SUCCESS] == 2
    assert generator_status_counts == Counter({"failed": 4, "done": 2})


def _assert_canonical_generator_task_id(
    task_id: str,
    attempt_id: str | None,
) -> None:
    assert attempt_id is not None
    assert task_id.startswith(f"{attempt_id}:gen:"), task_id


def _local_generator_task_id(task_id: str) -> str:
    return task_id.rsplit(":gen:", maxsplit=1)[-1]


def _assert_shared_bootstrap_conflicts(report: RunReport) -> None:
    bootstrap_write_errors = [
        call
        for call in report.tool_calls
        if call.tool_name == "write_file"
        and call.is_error
        and call.metadata.get("status") == "aborted_version"
        and call.metadata.get("conflict_reason") == "content changed"
    ]
    assert len(bootstrap_write_errors) == 4, [
        call.as_dict() for call in bootstrap_write_errors
    ]


async def _load_task_center_performance_report(
    report: RunReport,
) -> Mapping[str, Any]:
    task = report.performance_report_task
    assert task is not None, "run did not schedule performance_report.json"
    perf_path = await task
    assert perf_path == report.run_dir / "performance_report.json"
    return load_performance_report(report.run_dir)


def _max_overlapping_tool_calls(per_tool: Mapping[str, Any]) -> int:
    points: list[tuple[float, int]] = []
    for stats in per_tool.values():
        for sample in mapping(stats).get("samples") or ():
            sample_map = mapping(sample)
            started = sample_map.get("started_ts")
            completed = sample_map.get("completed_ts")
            if started is None or completed is None:
                continue
            points.append((_timestamp(started), 1))
            points.append((_timestamp(completed), -1))

    active = 0
    max_active = 0
    for _ts, delta in sorted(points, key=lambda item: (item[0], item[1])):
        active += delta
        max_active = max(max_active, active)
    return max_active


def _timestamp(raw: object) -> float:
    return datetime.fromisoformat(str(raw)).timestamp()

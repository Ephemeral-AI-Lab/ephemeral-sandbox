"""Capacity-suite regression for ``capacity.full_system_capacity_matrix``."""

from __future__ import annotations

import json
from collections import Counter
from pathlib import Path
from typing import Any

import pytest

import sandbox.api as sandbox_api
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.audit.events import EventType
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_capacity_enabled,
)
from task_center_runner.environments.sweevo_image.health import (
    require_sweevo_image_provider_healthy,
)
from sandbox.api import ReadFileRequest, SandboxCaller

pytestmark = [
    pytest.mark.asyncio,
    pytest.mark.live_e2e_capacity,
    pytest.mark.live_e2e_daytona,
]

_FORBIDDEN_RUN_SIGNATURES = (
    "internal_error",
    "manifest references missing layer",
    "stale lowerdir",
    ".pyright_scratch",
    "untyped conflict",
)


@pytest.mark.skipif(
    not live_e2e_capacity_enabled(),
    reason="capacity live e2e disabled in runner.live_e2e.capacity_enabled",
)
@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
async def test_full_system_capacity_matrix_records_artifacts_and_metrics(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    require_sweevo_image_provider_healthy(sweevo_image_instance)

    scenario = SCENARIO_REGISTRY["capacity.full_system_capacity_matrix"]()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.task_center_status == "done", report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]
    assert report.run_dir.parts[-3:-1] == (
        "scenario_logs",
        "capacity.full_system_capacity_matrix",
    )

    _assert_graph_shape(report.graph_summary)
    _assert_tool_and_event_capacity(report)
    _assert_audit_artifacts(report.run_dir)
    _assert_no_forbidden_signatures(report.run_dir)
    await _assert_capacity_workspace_artifacts(
        report.sandbox_id,
        report.task_center_run_id,
    )

def _assert_graph_shape(graph_summary: dict[str, Any]) -> None:
    workflows = graph_summary["workflows"]
    assert len(workflows) >= 2, graph_summary
    root = next(
        workflow
        for workflow in workflows
        if workflow.get("origin_kind") == "entry"
    )
    recursive = [
        workflow
        for workflow in workflows
        if workflow.get("origin_kind") == "task"
    ]
    assert recursive, graph_summary
    assert root["status"] == "succeeded"
    assert all(workflow["status"] == "succeeded" for workflow in recursive)

    attempts = [
        attempt
        for workflow in workflows
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
    ]
    tasks = [task for attempt in attempts for task in attempt["tasks"]]
    assert len(root["iterations"]) >= 3
    assert len(attempts) >= 5
    assert len(tasks) >= 20
    assert any(attempt["deferred_goal_for_next_iteration"] for attempt in attempts)
    assert any(
        task.get("id", "").endswith(":capacity_metrics_summary")
        and task.get("status") == "done"
        for task in tasks
    )
    assert any(
        task.get("agent_name") == "verifier" and task.get("status") == "failed"
        for task in tasks
    )
    assert any(
        task.get("agent_name") == "verifier" and len(task["needs"]) > 1
        for task in tasks
    )
    assert max(len(attempt["tasks"]) for attempt in attempts) >= 5


def _assert_tool_and_event_capacity(report: Any) -> None:
    tool_counts = Counter(call.tool_name for call in report.tool_calls)
    assert tool_counts["write_file"] >= 30
    assert tool_counts["edit_file"] >= 5
    assert tool_counts["read_file"] >= 20
    assert tool_counts["shell"] >= 10
    assert (
        sum(count for name, count in tool_counts.items() if name.startswith("lsp."))
        >= 5
    )

    required_events = {
        EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED,
        EventType.SANDBOX_OVERLAY_EXECUTED,
        EventType.SANDBOX_OCC_CHANGESET_RECEIVED,
        EventType.SANDBOX_OCC_CHANGES_COMMITTED,
        EventType.SANDBOX_CONFLICT_DETECTED,
    }
    seen = {event.type for event in report.events}
    missing = sorted(event.value for event in required_events - seen)
    assert not missing, f"missing required events: {missing}"
    assert int(report.metrics.get("tool_errors_total") or 0) >= 1


def _assert_audit_artifacts(run_dir: Path) -> None:
    run_payload = _json_file(run_dir / "run.json")
    assert run_payload["status"] == "finished"
    assert _json_file(run_dir / "metrics.json")["tool_calls_total"] > 0

    task_files = list(run_dir.rglob("task.json"))
    message_files = list(run_dir.rglob("message.jsonl"))
    assert task_files, f"no task.json files under {run_dir}"
    assert message_files, f"no message.jsonl files under {run_dir}"
    assert all(path.stat().st_size > 0 for path in message_files)

    sandbox_log = run_dir / "sandbox_events.jsonl"
    assert sandbox_log.exists()
    sandbox_events = [row["event_type"] for row in _jsonl_rows(sandbox_log)]
    assert EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED.value in sandbox_events
    assert EventType.SANDBOX_CONFLICT_DETECTED.value in sandbox_events


def _assert_no_forbidden_signatures(run_dir: Path) -> None:
    for path in [
        run_dir / "run.json",
        run_dir / "metrics.json",
        *run_dir.rglob("message.jsonl"),
    ]:
        text = path.read_text(encoding="utf-8")
        lowered = text.lower()
        for signature in _FORBIDDEN_RUN_SIGNATURES:
            assert signature.lower() not in lowered, f"{signature!r} appeared in {path}"


async def _assert_capacity_workspace_artifacts(
    sandbox_id: str,
    task_center_run_id: str,
) -> None:
    caller = SandboxCaller(agent_id="capacity-full-system-test")
    summary = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(
            path=(
                "/testbed/.ephemeralos/sweevo-mock/capacity/"
                "full-system-capacity-summary.json"
            ),
            caller=caller,
        ),
    )
    assert summary.success and summary.exists
    summary_payload = json.loads(summary.content)
    assert summary_payload["schema"] == "live_e2e.capacity.v1"
    assert summary_payload["scenario"] == "capacity.full_system_capacity_matrix"
    assert summary_payload["task_center_run_id"] == task_center_run_id
    assert summary_payload["graph"]["planned_matrix_cells"] >= 32
    assert summary_payload["tool_use"]["lsp"] >= 5

    planned_graph = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path="/testbed/.metrics/planned_graph.json", caller=caller),
    )
    assert planned_graph.success and planned_graph.exists
    graph_payload = json.loads(planned_graph.content)
    assert graph_payload["schema"] == "live_e2e.capacity.planned_graph.v1"
    assert graph_payload["matrix_cell_count"] >= 32
    assert ["capacity_metrics_summary", "final_release_guard"] in graph_payload[
        "final_edges"
    ]


def _json_file(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _jsonl_rows(path: Path) -> list[dict[str, Any]]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]

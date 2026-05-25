"""Shared assertions for 3.5 plugin/LSP live tests."""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path
from typing import Any

import sandbox.api as sandbox_api
from benchmarks.sweevo.models import SWEEvoInstance
from sandbox._shared.models import ReadFileRequest, SandboxCaller
from task_center_runner.core.runner import RunReport
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    jsonl_rows,
    load_performance_report,
    mapping,
)


async def run_plugin_scenario(
    *,
    scenario_name: str,
    summary_path: str,
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> tuple[RunReport, dict[str, Any]]:
    scenario = SCENARIO_REGISTRY[scenario_name]()
    sandbox_id = str(workspace["sandbox_id"])
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
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
    if report.performance_report_task is not None:
        await report.performance_report_task
    summary = await read_json_summary(sandbox_id, summary_path)
    (report.run_dir / "plugin_summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return report, summary


async def read_json_summary(sandbox_id: str, path: str) -> dict[str, Any]:
    read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(
            path=path,
            caller=SandboxCaller(agent_id="test.plugin.summary"),
        ),
    )
    assert read.success and read.exists, read
    summary = json.loads(read.content or "{}")
    assert isinstance(summary, dict), summary
    return summary


def assert_no_internal_sandbox_errors(run_dir: Path) -> None:
    events_path = run_dir / "sandbox_events.jsonl"
    assert events_path.exists(), events_path
    raw = events_path.read_text(encoding="utf-8", errors="replace")
    for needle in (
        "internal_error",
        "stale lowerdir",
        "manifest references missing layer",
        "mount_failed",
    ):
        assert needle not in raw, f"{needle!r} appears in {events_path}"


def assert_plugin_o1_artifacts(report: RunReport) -> Mapping[str, Any]:
    events_path = report.run_dir / "sandbox_events.jsonl"
    assert_o1_workspace_resource_snapshots(events_path)
    perf = load_performance_report(report.run_dir)
    resources = mapping(mapping(perf["sandbox"]).get("resource_keys") or {})
    for key in (
        "resource.command_exec.workspace_tree_bytes",
        "resource.command_exec.workspace_tree_exists",
    ):
        if key in resources:
            assert float(mapping(resources[key]).get("max") or 0.0) == 0.0
    return perf


def assert_sandbox_events_include(run_dir: Path, needle: str) -> None:
    rows = jsonl_rows(run_dir / "sandbox_events.jsonl")
    assert any(needle in json.dumps(row, sort_keys=True) for row in rows), needle


__all__ = [
    "assert_no_internal_sandbox_errors",
    "assert_plugin_o1_artifacts",
    "assert_sandbox_events_include",
    "read_json_summary",
    "run_plugin_scenario",
]

"""Shared assertions for 3.2 ephemeral-workspace live tests."""

from __future__ import annotations

import json
import statistics
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

import sandbox.api as sandbox_api
from sandbox.api import ReadFileRequest, SandboxCaller
from test_runner.benchmarks.sweevo.models import SWEEvoInstance
from test_runner.core.runner import RunReport
from test_runner.core.stores import TaskStoreBundle
from test_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from test_runner.scenarios import SCENARIO_REGISTRY
from test_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    assert_resource_key_max,
    assert_timing_keys_present,
    jsonl_rows,
    load_performance_report,
    mapping,
)

REQUIRED_OVERLAY_TIMING_KEYS = (
    "command_exec.capture_upperdir_s",
    "command_exec.occ_apply_s",
    "api.exec_command.dispatch_total_s",
    "api.exec_command.total_s",
)


async def run_ephemeral_scenario(
    *,
    scenario_name: str,
    summary_path: str,
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
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
    assert report.request_status == "done", report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]
    if report.performance_report_task is not None:
        await report.performance_report_task
    summary = await read_json_summary(sandbox_id, summary_path)
    return report, summary


async def read_json_summary(sandbox_id: str, path: str) -> dict[str, Any]:
    read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(
            path=path,
            caller=SandboxCaller(agent_id="test.ephemeral_workspace.summary"),
        ),
    )
    assert read.success and read.exists, read
    parsed = json.loads(read.content or "{}")
    assert isinstance(parsed, dict), parsed
    return parsed


def assert_no_internal_sandbox_errors(run_dir: Path) -> None:
    events_path = run_dir / "sandbox_events.jsonl"
    assert events_path.exists(), events_path
    raw = events_path.read_text(encoding="utf-8", errors="replace")
    forbidden = (
        "internal_error",
        "stale lowerdir",
        "manifest references missing layer",
        "mount_failed",
    )
    for needle in forbidden:
        assert needle not in raw, f"{needle!r} appears in {events_path}"


def assert_ephemeral_performance_artifacts(
    report: RunReport,
    *,
    extra_timing_keys: Sequence[str] = (),
    require_overlay_timings: bool = True,
) -> Mapping[str, Any]:
    events_path = report.run_dir / "sandbox_events.jsonl"
    assert_o1_workspace_resource_snapshots(events_path)
    perf = load_performance_report(report.run_dir)
    required = (
        (*REQUIRED_OVERLAY_TIMING_KEYS, *extra_timing_keys)
        if require_overlay_timings
        else tuple(extra_timing_keys)
    )
    assert_timing_keys_present(perf, required)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_bytes", 0.0)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_exists", 0.0)
    resources = mapping(mapping(perf["sandbox"])["resource_keys"])
    for key in (
        "resource.command_exec.run_dir_tree_truncated",
        "resource.command_exec.upperdir_tree_truncated",
        "resource.command_exec.workspace_tree_truncated",
    ):
        assert float(mapping(resources[key])["max"]) == 0.0
    return perf


def assert_warm_tool_budgets(perf: Mapping[str, Any]) -> None:
    per_tool = mapping(mapping(mapping(perf["tools"])["per_tool"]))
    budgets = {
        "read_file": 500.0,
        "grep": 500.0,
        "glob": 500.0,
        "write_file": 1_000.0,
        "edit_file": 1_000.0,
    }
    for tool_name, budget_ms in budgets.items():
        if tool_name not in per_tool:
            continue
        p95_ms = _warm_p95(mapping(per_tool[tool_name]))
        assert p95_ms <= budget_ms, (
            f"{tool_name} warm p95 {p95_ms:.3f}ms exceeds {budget_ms:.0f}ms"
        )


def assert_sandbox_events_have_source(
    run_dir: Path,
    *,
    mutation_source: str,
) -> None:
    rows = jsonl_rows(run_dir / "sandbox_events.jsonl")
    assert any(
        mapping(row.get("payload") or {}).get("mutation_source") == mutation_source
        for row in rows
    ), f"missing mutation_source={mutation_source!r}"


def _warm_p95(tool_stats: Mapping[str, Any]) -> float:
    samples = list(tool_stats.get("samples") or ())
    durations = [
        duration_ms
        for sample in samples[2:]
        if (duration_ms := _sample_duration_ms(mapping(sample))) is not None
    ]
    if not durations:
        return float(tool_stats.get("p95_ms") or 0.0)
    if len(durations) == 1:
        return durations[0]
    return float(statistics.quantiles(durations, n=20, method="inclusive")[18])


def _sample_duration_ms(sample: Mapping[str, Any]) -> float | None:
    duration_ms = sample.get("duration_ms")
    if duration_ms is not None:
        return float(duration_ms)
    timings = mapping(sample.get("timings_s") or {})
    for key in (
        "api.read.total_s",
        "api.write.total_s",
        "api.edit.total_s",
        "command_exec.total_s",
        "runtime.dispatch_s",
    ):
        if key in timings:
            return float(timings[key]) * 1000.0
    return None


__all__ = [
    "assert_ephemeral_performance_artifacts",
    "assert_no_internal_sandbox_errors",
    "assert_sandbox_events_have_source",
    "assert_warm_tool_budgets",
    "run_ephemeral_scenario",
]

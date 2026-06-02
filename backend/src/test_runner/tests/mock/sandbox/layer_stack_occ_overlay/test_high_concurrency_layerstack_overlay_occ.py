"""Heavy live regression for concurrent layer-stack, overlay, and OCC pressure."""

from __future__ import annotations

import json
from collections import Counter
from collections.abc import Mapping
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

import sandbox.api as sandbox_api
from test_runner.benchmarks.sweevo.models import SWEEvoInstance
from sandbox.api import ReadFileRequest, SandboxCaller
from test_runner.agent.mock.high_concurrency_probe import (
    CONFLICT_WORKER_COUNT,
    DATA_FILES_PER_WORKER,
    READS_PER_WORKER,
    SUMMARY_PATH,
    SUMMARY_SCHEMA,
)
from test_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from test_runner.core.runner import RunReport
from test_runner.core.stores import TaskStoreBundle
from test_runner.audit.events import EventType
from test_runner.scenarios import SCENARIO_REGISTRY
from test_runner.scenarios.sandbox.high_concurrency_layerstack_overlay_occ import (
    MAX_CONCURRENT_WORKERS,
    WORKER_COUNT,
)
from test_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from test_runner.tests.mock._focused_scenario_contracts import count_role_tasks
from test_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    assert_resource_key_max,
    assert_timing_keys_present,
    jsonl_rows,
    load_performance_report,
    mapping,
)


pytestmark = pytest.mark.asyncio

_WRITE_EDIT_P95_BUDGET_MS = 1_000.0
_REQUIRED_LATENCY_KEYS = (
    "command_exec.capture_upperdir_s",
    "command_exec.occ_apply_s",
    "occ.apply.commit_queue_wait_s",
    "occ.apply.commit_resume_wait_s",
    "occ.apply.total_s",
    "api.exec_command.dispatch_total_s",
    "api.exec_command.total_s",
)


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(1800)
async def test_high_concurrency_layerstack_overlay_occ_capacity(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
) -> None:
    scenario_cls = SCENARIO_REGISTRY["sandbox.high_concurrency_layerstack_overlay_occ"]
    scenario = scenario_cls()
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

    summary = await _read_summary(sandbox_id)
    _assert_summary(summary)
    _assert_report_shape(report, summary)
    _assert_sandbox_events(report.run_dir / "sandbox_events.jsonl", summary)
    await _assert_performance_report(report, summary)


def _assert_summary(summary: Mapping[str, Any]) -> None:
    assert summary["schema"] == SUMMARY_SCHEMA
    assert summary["worker_count"] == WORKER_COUNT
    assert summary["worker_indexes"] == list(range(WORKER_COUNT))
    assert int(summary["conflict_successes"]) >= 1
    assert int(summary["conflict_errors"]) >= 1
    assert (
        int(summary["conflict_successes"]) + int(summary["conflict_errors"])
        == CONFLICT_WORKER_COUNT
    )
    assert int(summary["total_write_calls"]) == WORKER_COUNT * DATA_FILES_PER_WORKER
    assert int(summary["total_edit_calls"]) == (
        WORKER_COUNT * DATA_FILES_PER_WORKER + CONFLICT_WORKER_COUNT
    )
    assert int(summary["total_read_calls"]) == WORKER_COUNT * READS_PER_WORKER
    assert int(summary["total_shell_calls"]) == 0


def _assert_report_shape(report: RunReport, summary: Mapping[str, Any]) -> None:
    counts = Counter(event.type for event in report.events)
    # Executor success is asserted through real store state (seed +
    # WORKER_COUNT workers + reconcile). SANDBOX_CONFLICT_DETECTED is still
    # emitted through ProbeContext.publish.
    assert count_role_tasks(report, "executor", status="done") >= WORKER_COUNT + 2
    assert counts[EventType.SANDBOX_CONFLICT_DETECTED] >= int(
        summary["conflict_errors"]
    )

    error_calls = [call for call in report.tool_calls if call.is_error]
    assert len(error_calls) == int(summary["conflict_errors"])
    assert {call.tool_name for call in error_calls} == {"edit_file"}
    for call in error_calls:
        status = str(call.metadata.get("status") or "")
        assert status != "internal_error", call.metadata
        assert str(call.metadata.get("conflict_reason") or ""), call.metadata

    tool_counts = Counter(call.tool_name for call in report.tool_calls)
    assert tool_counts["write_file"] >= (
        WORKER_COUNT * DATA_FILES_PER_WORKER + WORKER_COUNT + 3
    )
    assert tool_counts["edit_file"] >= (
        WORKER_COUNT * DATA_FILES_PER_WORKER + CONFLICT_WORKER_COUNT
    )
    assert tool_counts["read_file"] >= WORKER_COUNT * (READS_PER_WORKER + 1) + 1
    assert tool_counts["exec_command"] >= 1


def _assert_sandbox_events(path: Path, summary: Mapping[str, Any]) -> None:
    assert path.exists()
    rows = jsonl_rows(path)
    counts = Counter(row.get("event_type") for row in rows)
    assert counts[EventType.SANDBOX_OVERLAY_EXECUTED.value] >= 1
    assert counts[EventType.SANDBOX_OCC_CHANGESET_RECEIVED.value] >= (
        WORKER_COUNT * DATA_FILES_PER_WORKER
    )
    assert counts[EventType.SANDBOX_OCC_CHANGES_COMMITTED.value] >= (
        WORKER_COUNT * DATA_FILES_PER_WORKER
    )
    assert counts[EventType.SANDBOX_CONFLICT_DETECTED.value] >= int(
        summary["conflict_errors"]
    )
    assert_o1_workspace_resource_snapshots(path)


async def _assert_performance_report(
    report: RunReport,
    summary: Mapping[str, Any],
) -> None:
    assert report.performance_report_task is not None
    perf_path = await report.performance_report_task
    assert perf_path == report.run_dir / "performance_report.json"
    perf = load_performance_report(report.run_dir)
    assert_timing_keys_present(perf, _REQUIRED_LATENCY_KEYS)

    totals = mapping(perf["totals"])
    assert int(totals["tool_errors_total"]) == int(summary["conflict_errors"])
    assert int(totals["tool_calls_total"]) >= (
        WORKER_COUNT * (DATA_FILES_PER_WORKER * 2 + READS_PER_WORKER + 2)
    )

    per_tool = mapping(mapping(perf["tools"])["per_tool"])
    assert _max_overlapping_sandbox_tool_calls(per_tool) <= MAX_CONCURRENT_WORKERS
    write_stats = mapping(per_tool["write_file"])
    edit_stats = mapping(per_tool["edit_file"])
    shell_stats = mapping(per_tool["exec_command"])
    assert int(write_stats["count"]) >= (
        WORKER_COUNT * DATA_FILES_PER_WORKER + WORKER_COUNT + 2
    )
    assert int(edit_stats["errors"]) == int(summary["conflict_errors"])
    assert int(shell_stats["count"]) >= 1
    assert _worker_shell_sample_count(shell_stats) == 0
    for tool_name, stats in (("write_file", write_stats), ("edit_file", edit_stats)):
        p95_ms = float(stats["p95_ms"])
        assert p95_ms <= _WRITE_EDIT_P95_BUDGET_MS, (
            f"{tool_name} p95 {p95_ms:.3f}ms exceeds "
            f"{_WRITE_EDIT_P95_BUDGET_MS:.0f}ms budget"
        )

    sandbox = mapping(perf["sandbox"])
    event_counts = mapping(sandbox["event_type_counts"])
    assert int(event_counts[EventType.SANDBOX_RESOURCE_SNAPSHOT.value]) >= 1

    families = mapping(sandbox["families"])
    assert int(mapping(families["occ"])["conflict_count"]) >= int(
        summary["conflict_errors"]
    )
    assert int(mapping(families["overlay"])["event_count"]) >= 1
    assert int(mapping(families["layer_stack"])["event_count"]) >= 1

    resource_keys = mapping(sandbox["resource_keys"])
    assert "resource.layer_stack.manifest_depth" in resource_keys
    assert "resource.layer_stack.manifest_path_count" in resource_keys
    assert "resource.command_exec.changed_path_count" in resource_keys
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_bytes", 0.0)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_exists", 0.0)
    for key in (
        "resource.command_exec.run_dir_tree_truncated",
        "resource.command_exec.upperdir_tree_truncated",
        "resource.command_exec.workspace_tree_truncated",
    ):
        assert float(mapping(resource_keys[key])["max"]) == 0.0


def _max_overlapping_sandbox_tool_calls(per_tool: Mapping[str, Any]) -> int:
    points: list[tuple[float, int]] = []
    for tool_name in ("read_file", "write_file", "edit_file", "exec_command"):
        for sample in mapping(per_tool[tool_name]).get("samples") or ():
            sample_map = mapping(sample)
            points.append((_timestamp(sample_map["started_ts"]), 1))
            points.append((_timestamp(sample_map["completed_ts"]), -1))
    active = 0
    max_active = 0
    for _ts, delta in sorted(points, key=lambda item: (item[0], item[1])):
        active += delta
        max_active = max(max_active, active)
    return max_active


def _worker_shell_sample_count(shell_stats: Mapping[str, Any]) -> int:
    return sum(
        1
        for sample in shell_stats.get("samples") or ()
        if "concurrent_worker" in str(mapping(sample).get("agent_run_id") or "")
    )


def _timestamp(raw: object) -> float:
    return datetime.fromisoformat(str(raw)).timestamp()


async def _read_summary(sandbox_id: str) -> dict[str, Any]:
    caller = SandboxCaller(agent_id="sweevo-high-concurrency-test")
    result = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=SUMMARY_PATH, caller=caller),
    )
    assert result.success
    assert result.exists
    return json.loads(result.content)

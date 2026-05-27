"""Heavy live regression for long-running zoned shell-write lease/merge."""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path
from typing import Any

import pytest

import sandbox.api as sandbox_api
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from sandbox.api import ReadFileRequest, SandboxCaller
from task_center_runner.agent.mock.heavy_io_zoned_probe import (
    CHUNK_COUNT,
    CHUNK_MB,
    SUMMARY_PATH,
    SUMMARY_SCHEMA,
    ZONE_NAMES,
)
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.core.runner import RunReport
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.audit.events import EventType
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.scenarios.sandbox.heavy_io_zoned_concurrent import WORKER_COUNT
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    assert_resource_key_max,
    load_performance_report,
    mapping,
)


pytestmark = pytest.mark.asyncio

_UPPERDIR_PAYLOAD_OVERHEAD_BYTES = 4 * 1024 * 1024


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(1800)
async def test_heavy_io_zoned_concurrent(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario_cls = SCENARIO_REGISTRY["sandbox.heavy_io_zoned_concurrent"]
    scenario = scenario_cls()
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

    summary = await _read_summary(sandbox_id)
    _assert_summary(summary)
    _assert_zone_isolation(report, summary)
    _assert_o1_overlay(report)
    await _assert_upperdir_scales_with_payload(report, summary)


def _assert_summary(summary: Mapping[str, Any]) -> None:
    assert summary["schema"] == SUMMARY_SCHEMA
    assert summary["worker_count"] == WORKER_COUNT
    assert summary["worker_indexes"] == list(range(WORKER_COUNT))
    assert summary["chunk_count"] == CHUNK_COUNT
    assert summary["chunk_mb"] == CHUNK_MB
    per_zone = summary["per_zone"]
    assert set(per_zone.keys()) == set(ZONE_NAMES)
    for zone in ZONE_NAMES:
        bucket = per_zone[zone]
        assert bucket["merges_ok"] == WORKER_COUNT, zone
        assert bucket["file_count_sum"] == WORKER_COUNT * CHUNK_COUNT, zone


def _assert_zone_isolation(report: RunReport, summary: Mapping[str, Any]) -> None:
    """Outside-workspace shell writes must NOT appear in workspace OCC.

    Two invariants:

    1. The per-zone summary computed by the probe records
       ``outside_changed_paths == 0`` — outside-workspace shells should not
       contribute any paths to the workspace OCC changeset.
    2. Across every tool call recorded by the runner, no captured
       ``changed_paths`` entry should reference ``/tmp/`` (the outside-zone
       root). Any such path would mean the workspace overlay leaked
       outside-of-workspace state into OCC.
    """
    outside_bucket = summary["per_zone"]["outside"]
    assert int(outside_bucket["outside_changed_paths"]) == 0, (
        f"outside zone leaked into workspace OCC changed_paths: "
        f"count={outside_bucket['outside_changed_paths']}"
    )
    assert int(outside_bucket["workspace_changed_paths"]) == 0, (
        f"outside zone unexpectedly contributed workspace changed_paths: "
        f"count={outside_bucket['workspace_changed_paths']}"
    )

    tracked_bucket = summary["per_zone"]["gitincluded"]
    assert int(tracked_bucket["workspace_changed_paths"]) == WORKER_COUNT * CHUNK_COUNT

    gitignored_bucket = summary["per_zone"]["gitignored"]
    assert int(gitignored_bucket["workspace_changed_paths"]) == (
        WORKER_COUNT * CHUNK_COUNT
    )
    _assert_gitignore_route_filtering(report)

    for call in report.tool_calls:
        changed = list((call.metadata or {}).get("changed_paths") or ())
        leaked = [
            path for path in changed if str(path).startswith("/tmp/")
        ]
        assert not leaked, (
            f"tool call leaked /tmp paths into workspace OCC: "
            f"tool={call.tool_name} leaked={leaked}"
        )


def _assert_gitignore_route_filtering(report: RunReport) -> None:
    tracked_shells = 0
    gitignored_shells = 0
    for call in report.tool_calls:
        if call.tool_name != "shell":
            continue
        changed = [str(path) for path in (call.metadata or {}).get("changed_paths", ())]
        timings = (call.metadata or {}).get("timings") or {}
        if any(path.startswith("perf_load_tracked/") for path in changed):
            tracked_shells += 1
            assert float(timings.get("occ.commit.gated_path_count", 0.0)) == float(
                CHUNK_COUNT
            )
            assert float(timings.get("occ.commit.direct_path_count", 0.0)) == 0.0
        if any(path.startswith("build/perf_load_") for path in changed):
            gitignored_shells += 1
            assert float(timings.get("occ.commit.direct_path_count", 0.0)) == float(
                CHUNK_COUNT
            )
            assert float(timings.get("occ.commit.gated_path_count", 0.0)) == 0.0

    assert tracked_shells == WORKER_COUNT
    assert gitignored_shells == WORKER_COUNT


def _assert_o1_overlay(report: RunReport) -> None:
    """workspace_tree_bytes must stay 0 across the run (O(1) overlay disk)."""
    events_path = report.run_dir / "sandbox_events.jsonl"
    assert events_path.exists()
    assert_o1_workspace_resource_snapshots(events_path)
    max_workspace_bytes = 0.0
    max_workspace_exists = 0.0
    for line in events_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("event_type") != EventType.SANDBOX_RESOURCE_SNAPSHOT.value:
            continue
        payload = row.get("payload") or {}
        resources = payload.get("resources") or payload
        max_workspace_bytes = max(
            max_workspace_bytes,
            float(resources.get("resource.command_exec.workspace_tree_bytes", 0) or 0),
        )
        max_workspace_exists = max(
            max_workspace_exists,
            float(
                resources.get("resource.command_exec.workspace_tree_exists", 0) or 0
            ),
        )
    assert max_workspace_bytes == 0.0, (
        f"O(1) overlay regression: workspace_tree_bytes max={max_workspace_bytes}"
    )
    assert max_workspace_exists == 0.0, (
        f"O(1) overlay regression: workspace_tree_exists max={max_workspace_exists}"
    )


async def _assert_upperdir_scales_with_payload(
    report: RunReport,
    summary: Mapping[str, Any],
) -> None:
    assert report.performance_report_task is not None
    perf_path = await report.performance_report_task
    assert perf_path == report.run_dir / "performance_report.json"
    perf = load_performance_report(report.run_dir)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_bytes", 0.0)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_exists", 0.0)

    expected_payload_bytes = (
        int(summary["expected_kib_per_zone_per_worker"]) * 1024
    )
    resources = mapping(mapping(perf["sandbox"])["resource_keys"])
    upperdir_stats = mapping(resources["resource.command_exec.upperdir_tree_bytes"])
    changed_path_stats = mapping(resources["resource.command_exec.changed_path_count"])
    upperdir_max = float(upperdir_stats["max"])
    assert upperdir_max >= expected_payload_bytes * 0.95, (
        f"upperdir max {upperdir_max} did not capture tracked payload "
        f"{expected_payload_bytes}"
    )
    assert upperdir_max <= (
        expected_payload_bytes + _UPPERDIR_PAYLOAD_OVERHEAD_BYTES
    ), (
        f"upperdir max {upperdir_max} exceeds payload-proportional budget "
        f"{expected_payload_bytes + _UPPERDIR_PAYLOAD_OVERHEAD_BYTES}"
    )
    assert float(changed_path_stats["max"]) == float(CHUNK_COUNT)


async def _read_summary(sandbox_id: str) -> dict[str, Any]:
    caller = SandboxCaller(agent_id="sweevo-heavy-io-zoned-test")
    result = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=SUMMARY_PATH, caller=caller),
    )
    assert result.success
    assert result.exists
    return json.loads(result.content)

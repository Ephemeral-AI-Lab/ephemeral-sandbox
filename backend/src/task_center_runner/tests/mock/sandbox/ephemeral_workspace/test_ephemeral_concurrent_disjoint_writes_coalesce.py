"""3.2.2 concurrent disjoint ephemeral writes live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.ephemeral_workspace_probe import (
    CONCURRENT_WRITES_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.ephemeral_workspace._ephemeral_workspace_invariants import (
    assert_ephemeral_performance_artifacts,
    assert_no_internal_sandbox_errors,
    assert_sandbox_events_have_source,
    run_ephemeral_scenario,
)

pytestmark = [
    pytest.mark.asyncio,
    pytest.mark.skipif(not database_configured(), reason="database URL not configured"),
    pytest.mark.skipif(
        not live_e2e_heavy_enabled(),
        reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
    ),
]


@pytest.mark.timeout(900)
async def test_ephemeral_concurrent_disjoint_writes_coalesce(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_ephemeral_scenario(
        scenario_name="sandbox.ephemeral_workspace_concurrent_writes",
        summary_path=CONCURRENT_WRITES_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "concurrent_writes"
    assert summary["typed_write_count"] == 8
    assert summary["shell_write_count"] == 2
    assert set(summary["typed_sources"]) == {"api_write"}
    assert set(summary["shell_sources"]) == {"overlay_capture"}
    assert summary["runtime_after"]["command_overlay_run_dirs"] == 0
    for index in range(8):
        assert f"typed={index}" in summary["readbacks"][f"typed-{index}.txt"]
    for index in range(2):
        assert f"shell={index}" in summary["readbacks"][f"shell-{index}.txt"]

    assert_ephemeral_performance_artifacts(
        report,
        extra_timing_keys=("api.shell.total_s",),
    )
    assert_sandbox_events_have_source(report.run_dir, mutation_source="api_write")
    assert_sandbox_events_have_source(report.run_dir, mutation_source="overlay_capture")
    assert_no_internal_sandbox_errors(report.run_dir)

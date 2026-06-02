"""3.2.6 lowerdir disk O(1) under 100 ephemeral calls."""

from __future__ import annotations

from pathlib import Path

import pytest

from test_runner.benchmarks.sweevo.models import SWEEvoInstance
from test_runner.agent.mock.ephemeral_workspace_probe import O1_DISK_SUMMARY
from test_runner.core.stores import TaskStoreBundle
from test_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from test_runner.tests.mock.sandbox.ephemeral_workspace._ephemeral_workspace_invariants import (
    assert_ephemeral_performance_artifacts,
    assert_no_internal_sandbox_errors,
    assert_warm_tool_budgets,
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


@pytest.mark.timeout(1800)
async def test_ephemeral_lowerdir_disk_is_o1_under_100_calls(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
) -> None:
    report, summary = await run_ephemeral_scenario(
        scenario_name="sandbox.ephemeral_workspace_o1_disk",
        summary_path=O1_DISK_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "o1_disk"
    assert summary["operation_count"] == 100
    assert summary["manifest_delta"] >= summary["mutation_count"]
    assert summary["auto_squash_count"] >= 0
    assert summary["tool_counts"]["write_file"] >= 30
    assert summary["tool_counts"]["edit_file"] >= 30
    assert summary["tool_counts"]["read_file"] >= 30
    active_leases = [
        int(sample["layer_metrics"]["active_leases"]) for sample in summary["samples"]
    ]
    for sample in summary["samples"]:
        assert sample["runtime"]["command_overlay_run_dirs"] == 0, sample
        assert sample["layer_metrics"]["active_leases"] <= 1, sample
    assert max(active_leases) <= 1, summary
    assert int(summary["final_layer_metrics"]["active_leases"]) <= 1, summary
    assert summary["warm_p95_ms"]["read_file"] <= 500.0
    assert summary["warm_p95_ms"]["write_file"] <= 1_000.0
    assert summary["warm_p95_ms"]["edit_file"] <= 1_000.0

    perf = assert_ephemeral_performance_artifacts(
        report,
        require_overlay_timings=False,
    )
    assert_warm_tool_budgets(perf)
    assert_no_internal_sandbox_errors(report.run_dir)

"""3.5.1 READ_ONLY LSP refresh without per-call publish."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.plugin_workspace_probe import (
    READ_ONLY_LSP_REFRESH_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.plugin._plugin_invariants import (
    assert_no_internal_sandbox_errors,
    assert_plugin_o1_artifacts,
    run_plugin_scenario,
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
async def test_plugin_read_only_lsp_refresh_without_publish(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_plugin_scenario(
        scenario_name="sandbox.plugin_read_only_lsp_refresh",
        summary_path=READ_ONLY_LSP_REFRESH_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["schema"] == "task_center_runner.plugin_workspace.v1"
    assert summary["mode"] == "read_only_lsp_refresh"
    assert summary["lsp_read_only_publish_count"] == 0
    assert summary["lsp_overlay_publish_timing_count"] == 0
    assert summary["diagnostics_after_count"] > 0
    assert "missing_symbol" in summary["diagnostics_after_text"]
    assert summary["read_after_contains_missing_symbol"] is True
    assert summary["start_delta_after_edit"] == 0.0
    assert summary["refresh_total_after_edit"] >= 1.0
    assert summary["warm_lsp_p95_ms"] <= 500.0

    assert_plugin_o1_artifacts(report)
    assert_no_internal_sandbox_errors(report.run_dir)

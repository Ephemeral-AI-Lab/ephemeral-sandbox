"""3.5.2 WRITE_ALLOWED plugin WorkspaceEdit publish path."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.plugin_workspace_probe import (
    WRITE_ALLOWED_PUBLISH_SUMMARY,
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
async def test_plugin_write_allowed_apply_workspace_edit_publishes(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_plugin_scenario(
        scenario_name="sandbox.plugin_write_allowed_publish",
        summary_path=WRITE_ALLOWED_PUBLISH_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["schema"] == "task_center_runner.plugin_workspace.v1"
    assert summary["mode"] == "write_allowed_publish"
    assert summary["apply_result"]["success"] is True
    assert summary["apply_manifest_version"] is not None
    assert any(path.endswith("target.py") for path in summary["apply_changed_paths"])
    assert summary["apply_overlay_timing_keys"], summary["records"]
    assert summary["normal_read_has_new_value"] is True
    if summary["runtime_before"]["command_overlay_run_dirs"] == 0:
        assert summary["runtime_after"]["command_overlay_run_dirs"] == 0
    assert summary["command_overlay_run_dir_delta"] <= 0

    assert_plugin_o1_artifacts(report)
    assert_no_internal_sandbox_errors(report.run_dir)

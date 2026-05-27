"""3.5.4 plugin dispatch is blocked while isolated_workspace is open."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.plugin_workspace_probe import IWS_POLICY_SUMMARY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.background_tool._background_shell_invariants import (
    configure_isolated_workspace_for_background,
)
from task_center_runner.tests.mock.sandbox.plugin._plugin_invariants import (
    assert_no_internal_sandbox_errors,
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
async def test_plugin_blocked_in_open_isolated_workspace(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    await configure_isolated_workspace_for_background(str(workspace["sandbox_id"]))
    report, summary = await run_plugin_scenario(
        scenario_name="sandbox.plugin_iws_policy",
        summary_path=IWS_POLICY_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["schema"] == "task_center_runner.plugin_workspace.v1"
    assert summary["mode"] == "iws_policy"
    assert summary["enter"]["is_error"] is False
    assert summary["exit"]["is_error"] is False
    assert summary["blocked_status"]["kind"] == "forbidden_in_isolated_workspace"
    assert summary["blocked_lsp"]["kind"] == "forbidden_in_isolated_workspace"
    assert summary["default_status_success"] is True
    assert_no_internal_sandbox_errors(report.run_dir)

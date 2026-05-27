"""3.5.5 plugin setup/network failures are actionable and retryable."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.plugin_workspace_probe import SETUP_FAILURE_SUMMARY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
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


@pytest.mark.timeout(420)
async def test_plugin_setup_network_failure_is_actionable(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_plugin_scenario(
        scenario_name="sandbox.plugin_setup_failure",
        summary_path=SETUP_FAILURE_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    failure = summary["failure"]
    details = failure["metadata"]["details"]
    retry = summary["retry"]

    assert summary["schema"] == "task_center_runner.plugin_workspace.v1"
    assert summary["mode"] == "setup_failure"
    assert failure["is_error"] is True
    assert failure["metadata"]["step"] == "install"
    assert failure["metadata"]["error_kind"] == "plugin_setup_network_failure"
    assert details["plugin"] == "netfail"
    assert details["setup_step"] == "setup.sh"
    assert "registry.npmjs.org" in details["command"]
    assert "Could not resolve host" in details["stderr_excerpt"]
    assert retry["is_error"] is False
    assert retry["install_attempts"] == 1
    assert retry["dispatch_calls"] == ["api.plugin.ensure", "plugin.netfail.run"]
    assert_no_internal_sandbox_errors(report.run_dir)

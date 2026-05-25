"""3.5.3 plugin intent registration and dispatch contracts."""

from __future__ import annotations

from pathlib import Path

import pytest

from benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.plugin_workspace_probe import INTENT_CONTRACT_SUMMARY
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
async def test_plugin_intent_mislabel_fails_fast(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_plugin_scenario(
        scenario_name="sandbox.plugin_intent_contract",
        summary_path=INTENT_CONTRACT_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["schema"] == "task_center_runner.plugin_workspace.v1"
    assert summary["mode"] == "intent_contract"
    assert summary["missing_intent_error"]["type"] == "TypeError"
    assert summary["lifecycle_error"]["type"] == "PluginOpRegistrationError"
    assert summary["read_only_result"]["path"] == "service"
    assert summary["write_allowed_result"]["path"] == "overlay"
    assert summary["write_allowed_result"]["overlay_runner_used"] is True
    assert summary["overlay_calls"] == [{"plugin": "demo", "op": "write"}]
    assert_no_internal_sandbox_errors(report.run_dir)

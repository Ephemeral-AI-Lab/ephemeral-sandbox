"""3.2.4 outside-workspace policy live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from test_runner.benchmarks.sweevo.models import SWEEvoInstance
from test_runner.agent.mock.ephemeral_workspace_probe import POLICY_SUMMARY
from test_runner.core.stores import TaskStoreBundle
from test_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from test_runner.tests.mock.sandbox.ephemeral_workspace._ephemeral_workspace_invariants import (
    assert_ephemeral_performance_artifacts,
    assert_no_internal_sandbox_errors,
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


@pytest.mark.timeout(600)
async def test_ephemeral_outside_workspace_policy(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
) -> None:
    report, summary = await run_ephemeral_scenario(
        scenario_name="sandbox.ephemeral_workspace_policy",
        summary_path=POLICY_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "policy"
    assert summary["hosts_read_ok"] is True
    assert summary["tmp_write_changed_paths"] == []
    assert "tmp-ok" in summary["tmp_probe_stdout"]
    assert summary["outside_command_has_public_timing"] is True, summary
    assert summary["outside_command_has_capture_timing"] is True, summary

    assert_ephemeral_performance_artifacts(
        report,
        extra_timing_keys=(
            "api.exec_command.dispatch_total_s",
            "api.shell.total_s",
            "command_exec.capture_upperdir_s",
        ),
        require_overlay_timings=False,
    )
    assert_no_internal_sandbox_errors(report.run_dir)

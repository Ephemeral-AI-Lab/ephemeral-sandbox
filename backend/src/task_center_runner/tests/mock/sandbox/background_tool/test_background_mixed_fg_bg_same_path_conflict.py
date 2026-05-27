"""3.4.1 foreground/background same-path conflict live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.background_shell_probe import (
    MIXED_CONFLICT_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.background_tool._background_shell_invariants import (
    assert_background_performance_artifacts,
    run_background_shell_scenario,
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
async def test_background_mixed_fg_bg_same_path_conflict(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_background_shell_scenario(
        scenario_name="sandbox.background_mixed_fg_bg_same_path_conflict",
        summary_path=MIXED_CONFLICT_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "mixed_fg_bg_same_path_conflict", summary
    assert not summary["foreground"]["is_error"], summary
    assert summary["background"]["is_error"], summary
    assert summary["foreground_won"], summary
    assert not summary["background_won"], summary
    assert summary["background"]["status"] in {
        "aborted_lock",
        "aborted_overlap",
        "aborted_version",
        "error",
        "failed",
        None,
    }, summary
    mount_s = float(
        summary["background"]["shell_metadata"]["timings"].get(
            "command_exec.mount_workspace_s",
            0.0,
        )
    )
    assert mount_s < 5.0, summary
    write_total_s = float(
        summary["foreground"]["metadata"]["timings"].get("api.write.total_s", 0.0)
    )
    assert write_total_s < 5.0, summary

    assert_background_performance_artifacts(report)

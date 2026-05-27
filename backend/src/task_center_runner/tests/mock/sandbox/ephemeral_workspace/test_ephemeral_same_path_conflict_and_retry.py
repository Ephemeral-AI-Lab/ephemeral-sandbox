"""3.2.3 same-path conflict and retry live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.ephemeral_workspace_probe import ROOT
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.ephemeral_workspace._ephemeral_workspace_invariants import (
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


@pytest.mark.timeout(900)
async def test_ephemeral_same_path_conflict_and_retry(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_ephemeral_scenario(
        scenario_name="sandbox.ephemeral_workspace_same_path_conflict",
        summary_path=f"{ROOT}/same_path_conflict/summary.json",
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    first_wave = summary["first_wave"]
    assert sum(1 for item in first_wave if not item["is_error"]) >= 1
    conflicts = [item for item in first_wave if item["is_error"]]
    assert conflicts, first_wave
    for conflict in conflicts:
        assert conflict["status"] in {
            "aborted_overlap",
            "aborted_version",
            "failed",
            "rejected",
        }, conflict
        assert conflict["conflict_reason"] or conflict["status"], conflict
    assert summary["retry_records"], summary
    assert summary["last_successful_value"] in summary["final_content"]

    assert_ephemeral_performance_artifacts(
        report,
        require_overlay_timings=False,
    )
    assert_no_internal_sandbox_errors(report.run_dir)

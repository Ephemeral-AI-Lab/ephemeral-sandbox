"""3.4.2 heartbeat-loss live regression for background shell invocations."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.background_shell_probe import (
    HEARTBEAT_LOSS_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.background_tool._background_shell_invariants import (
    assert_background_performance_artifacts,
    configure_short_inflight_ttl,
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


@pytest.mark.timeout(720)
async def test_background_heartbeat_loss_reaps_only_stale_bg(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    sandbox_id = str(workspace["sandbox_id"])
    await configure_short_inflight_ttl(sandbox_id)

    report, summary = await run_background_shell_scenario(
        scenario_name="sandbox.background_heartbeat_loss_reaps_only_stale_bg",
        summary_path=HEARTBEAT_LOSS_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
        preserve_inflight_ttl=True,
    )

    assert summary["mode"] == "heartbeat_loss", summary
    assert summary["inflight_during_launch"] >= 2, summary
    assert summary["heartbeat_response_count"] > 0, summary
    assert summary["heartbeat_touched_total"] > 0, summary
    assert not summary["foreground"]["is_error"], summary
    assert not summary["protected"]["is_error"], summary
    assert summary["protected_published"], summary
    assert summary["stale"]["is_error"] or summary["stale"]["cancelled"], summary
    assert not summary["stale_published"], summary
    assert summary["inflight_after"] == 0, summary

    assert_background_performance_artifacts(report)

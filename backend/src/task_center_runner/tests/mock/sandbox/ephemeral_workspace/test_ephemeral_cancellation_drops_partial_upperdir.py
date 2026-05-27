"""3.2.5 cancellation drops partial upperdir live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.ephemeral_workspace_probe import CANCELLATION_SUMMARY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock._layer_stack_occ_overlay_assertions import jsonl_rows
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
async def test_ephemeral_cancellation_drops_partial_upperdir(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_ephemeral_scenario(
        scenario_name="sandbox.ephemeral_workspace_cancellation",
        summary_path=CANCELLATION_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "cancellation"
    assert summary["cancelled"] is True
    assert summary["partial_read_is_error"] is True
    assert summary["runtime_after"]["command_overlay_run_dirs"] == 0
    assert "ok" in summary["health_read"]

    rows = jsonl_rows(report.run_dir / "sandbox_events.jsonl")
    cancellation_events = [
        row for row in rows if row.get("event_type") == "sandbox_tool_cancelled"
    ]
    assert cancellation_events, "missing sandbox_tool_cancelled evidence"
    assert any(
        row.get("payload", {}).get("background_task_id")
        == summary["background_task_id"]
        and row.get("payload", {}).get("invocation_id")
        for row in cancellation_events
    )

    assert_ephemeral_performance_artifacts(
        report,
        require_overlay_timings=False,
    )
    assert_no_internal_sandbox_errors(report.run_dir)

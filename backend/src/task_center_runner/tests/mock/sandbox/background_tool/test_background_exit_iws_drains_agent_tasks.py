"""3.4.3 background lifecycle interaction with isolated workspace exit."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.background_shell_probe import (
    EXIT_IWS_DRAIN_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.background_tool._background_shell_invariants import (
    assert_background_performance_artifacts,
    configure_isolated_workspace_for_background,
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
async def test_background_exit_iws_drains_agent_tasks(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    sandbox_id = str(workspace["sandbox_id"])
    await configure_isolated_workspace_for_background(sandbox_id)

    report, summary = await run_background_shell_scenario(
        scenario_name="sandbox.background_exit_iws_drains_agent_tasks",
        summary_path=EXIT_IWS_DRAIN_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "exit_iws_drain", summary
    assert summary["default_inflight"] >= 1, summary
    # Enter is refused at the bg prehook (a hook_failure ToolResult), so the
    # reason tag lives in the hook trace, not a LifecycleError payload.
    assert summary["blocked_enter"]["is_error"], summary
    assert summary["blocked_enter_reason"] == "ephemeral_jobs_in_flight", summary
    assert summary["default_background"]["cancelled"], summary
    assert not summary["default_published"], summary
    assert not summary["iws_enter"]["is_error"], summary
    # Exit is now GATED: the first attempt is refused while the iws bg task is
    # in flight; the agent cancels it via cancel_background_task, then exit
    # succeeds. (Drain stays as defense-in-depth but no longer fires here.)
    assert summary["blocked_exit"]["is_error"], summary
    assert summary["blocked_exit_reason"] == "ephemeral_jobs_in_flight", summary
    assert not summary["cancel_bg"]["is_error"], summary
    assert not summary["iws_exit"]["is_error"], summary
    phases = summary["iws_exit_payload"]["phases_ms"]
    assert "evicted_background_tasks" in phases, summary
    # The local bridge task can complete after it observes the real background
    # task's terminal cancel/fail result; the invariant is that it is settled.
    assert summary["tracked_status_after_exit"] in {"cancelled", "completed", "failed"}, summary
    assert not summary["iws_published"], summary

    assert_background_performance_artifacts(report)

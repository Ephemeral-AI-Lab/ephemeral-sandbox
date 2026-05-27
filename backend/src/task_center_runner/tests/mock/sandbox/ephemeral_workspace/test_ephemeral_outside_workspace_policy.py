"""3.2.4 outside-workspace policy live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.ephemeral_workspace_probe import POLICY_SUMMARY
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


@pytest.mark.timeout(600)
async def test_ephemeral_outside_workspace_policy(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
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
    assert set(summary["denied"]) == {
        "/etc/hosts",
        "/proc/sysrq-trigger",
        "/sys/kernel/printk",
        "/boot/grub.cfg",
    }
    for path, result in summary["denied"].items():
        assert result["is_error"] is True, (path, result)
        assert result["error_kind"] == "forbidden_host_path", (path, result)
        assert result["changed_paths"] == [], (path, result)
        assert result["has_mount_timing"] is True, (path, result)

    assert_ephemeral_performance_artifacts(
        report,
        extra_timing_keys=("api.read.total_s", "api.write.total_s"),
    )
    raw_events = (report.run_dir / "sandbox_events.jsonl").read_text(
        encoding="utf-8",
        errors="replace",
    )
    assert "forbidden_host_path" in raw_events
    assert_no_internal_sandbox_errors(report.run_dir)

"""3.4.5 dispatcher responsiveness under many small background writes."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.background_shell_probe import (
    MANY_SMALL_WRITES_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.background_tool._background_shell_invariants import (
    assert_background_performance_artifacts,
    run_background_shell_scenario,
    tool_p95_ms,
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
async def test_background_many_small_writes_do_not_starve_dispatcher(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_background_shell_scenario(
        scenario_name="sandbox.background_many_small_writes_do_not_starve_dispatcher",
        summary_path=MANY_SMALL_WRITES_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["mode"] == "many_small_writes", summary
    assert summary["background_success_count"] == summary["background_count"], summary
    assert summary["foreground_p95_s"] < 5.0, summary
    assert summary["inflight_after"] == 0, summary
    assert len(summary["verified_background_files"]) == summary["background_count"]
    for record in summary["verified_background_files"]:
        assert not record["is_error"], record
        assert "bg-" in record["content"], record
    for record in summary["foreground"]:
        assert not record["write"]["is_error"], record
        assert not record["read_is_error"], record

    perf = assert_background_performance_artifacts(report)
    assert tool_p95_ms(perf, "read_file") < 5_000.0
    assert tool_p95_ms(perf, "write_file") < 5_000.0

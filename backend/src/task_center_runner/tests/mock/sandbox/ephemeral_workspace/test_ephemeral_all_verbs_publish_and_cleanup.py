"""3.2.1 all-verbs publish and cleanup live regression."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.ephemeral_workspace_probe import ALL_VERBS_SUMMARY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.ephemeral_workspace._ephemeral_workspace_invariants import (
    assert_ephemeral_performance_artifacts,
    assert_no_internal_sandbox_errors,
    assert_sandbox_events_have_source,
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
async def test_ephemeral_all_verbs_publish_and_cleanup(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report, summary = await run_ephemeral_scenario(
        scenario_name="sandbox.ephemeral_workspace_all_verbs",
        summary_path=ALL_VERBS_SUMMARY,
        sweevo_image_instance=sweevo_image_instance,
        workspace=workspace,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert summary["schema"] == "task_center_runner.ephemeral_workspace.v1"
    assert summary["mode"] == "all_verbs"
    assert summary["read_only_publish_count"] == 0
    labels = {record["label"] for record in summary["records"]}
    assert {"write_module", "read_module", "edit_module", "grep_beta", "glob_python", "shell_kinds"} <= labels
    for record in summary["records"]:
        assert record["runtime_after"]["command_overlay_run_dirs"] == 0, record
    assert {"write", "delete", "symlink", "opaque_dir"} <= set(
        summary["required_shell_kinds"]
    )

    perf = assert_ephemeral_performance_artifacts(
        report,
        extra_timing_keys=("api.shell.total_s", "api.grep.total_s", "api.glob.total_s"),
    )
    del perf
    assert_sandbox_events_have_source(report.run_dir, mutation_source="api_write")
    assert_sandbox_events_have_source(report.run_dir, mutation_source="overlay_capture")
    assert_no_internal_sandbox_errors(report.run_dir)

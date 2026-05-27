"""T6 — Partial-write cancel via the scenario harness."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import sandbox.api as sandbox_api
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from sandbox.shared.models import ReadFileRequest, SandboxCaller
from task_center_runner.agent.mock.background_shell_probe import (
    PARTIAL_WRITE_SUMMARY,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(420)
async def test_background_shell_partial_write_cancel(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario_cls = SCENARIO_REGISTRY[
        "sandbox.background_shell_partial_write_cancel"
    ]
    sandbox_id = str(workspace["sandbox_id"])
    report = await run_scenario_on_sweevo_image(
        scenario_cls(),
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        stores=stores,
    )
    assert report.task_center_status == "done", report

    read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(
            path=PARTIAL_WRITE_SUMMARY,
            caller=SandboxCaller(
                agent_id="test.background_shell_partial_write_cancel.read"
            ),
        ),
    )
    assert read.success and read.exists, read
    summary = json.loads(read.content or "{}")
    assert summary["mode"] == "partial_write_cancel", summary
    assert not summary["dd_completed_before_cancel"], (
        "dd completed before cancel — increase count or reduce cancel deadline"
    )
    # AC-6: cancelled partial write must not leave the file in workspace OCC.
    assert not summary["tracked_exists_after_cancel"], summary

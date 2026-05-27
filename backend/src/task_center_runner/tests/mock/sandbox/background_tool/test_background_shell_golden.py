"""T1 — Golden path live regression for ``shell(background=True)``.

Drives ``BackgroundShellGolden`` through the mock-agent scenario harness so
the run produces full ``.sweevo_runs/scenario_logs/.../sandbox_events.jsonl``
plus ``performance_report.json`` artifacts. The probe launches 3 concurrent
background shells (sleep 5 s, echo done), waits for natural exit, and writes
a JSON summary the test reads back via ``sandbox_api.read_file``.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import sandbox.api as sandbox_api
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from sandbox.shared.models import ReadFileRequest, SandboxCaller
from task_center_runner.agent.mock.background_shell_probe import GOLDEN_SUMMARY
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
async def test_background_shell_golden(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario_cls = SCENARIO_REGISTRY["sandbox.background_shell_golden"]
    scenario = scenario_cls()
    sandbox_id = str(workspace["sandbox_id"])
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        stores=stores,
    )
    assert report.task_center_status == "done", report

    read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(
            path=GOLDEN_SUMMARY,
            caller=SandboxCaller(agent_id="test.background_shell_golden.read"),
        ),
    )
    assert read.success and read.exists, read
    summary = json.loads(read.content or "{}")
    assert summary["mode"] == "golden", summary
    launches = summary["launches"]
    assert len(launches) == summary["launch_count"], summary
    for record in launches:
        assert record["exit_code"] == 0, record
        assert not record["is_error"], record

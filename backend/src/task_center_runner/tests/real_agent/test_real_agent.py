"""Real-agent live-e2e smoke test for a canonical SWE-EVO instance.

Run explicitly through the ``tests/real_agent`` suite. The test depends on the
function-scoped ``workspace`` fixture (per-test reset) rather than the
session-scoped ``sweevo_image_sandbox`` fixture to avoid cross-instance state
leakage when the test grows to a parameterized matrix.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.core.real_agent_run import run_sweevo_real_agent
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import real_agent_max_duration_s

pytestmark = pytest.mark.real_agent


@pytest.mark.asyncio
async def test_real_agent_resolves_canonical_instance(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    report = await run_sweevo_real_agent(
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
        max_duration_s=real_agent_max_duration_s(),
    )
    assert report.task_center_run_id
    assert report.run_dir.is_dir()
    assert (report.run_dir / "run.json").is_file()
    assert (report.run_dir / "sweevo_result.json").is_file()
    assert report.task_center_status in {"done", "failed", "cancelled"}
    if report.task_center_status == "done" and not report.aborted_by_timeout:
        assert report.sweevo_result.fail_to_pass_total > 0

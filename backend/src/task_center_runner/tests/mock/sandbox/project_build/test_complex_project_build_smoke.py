"""Smoke live regression for the complex project-build scenario."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.tests.mock._project_build_contracts import (
    assert_complex_build_smoke_contract,
)
from task_center_runner.tests._live_config import database_configured


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.timeout(900)
async def test_complex_project_build_smoke(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario_cls = SCENARIO_REGISTRY["sandbox.complex_project_build_smoke"]
    scenario = scenario_cls()
    sandbox_id = str(workspace["sandbox_id"])
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        stores=stores,
    )
    await assert_complex_build_smoke_contract(
        report=report,
        sandbox_id=sandbox_id,
    )

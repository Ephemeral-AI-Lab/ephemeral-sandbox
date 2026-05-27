"""3.6.3 shell/edit changes refresh LSP by remounting, not restarting."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock._project_build_contracts import (
    assert_project_build_shell_edit_lsp_remount_not_restart,
    assert_shell_edit_lsp_full_contract,
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
@pytest.mark.timeout(3600)
async def test_project_build_shell_edit_lsp_remount_not_restart(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY["sandbox.complex_project_build_shell_edit_lsp"]()
    sandbox_id = str(workspace["sandbox_id"])
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        stores=stores,
    )

    await assert_shell_edit_lsp_full_contract(
        report=report,
        sandbox_id=sandbox_id,
    )
    await assert_project_build_shell_edit_lsp_remount_not_restart(
        report,
        sandbox_id=sandbox_id,
    )

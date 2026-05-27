"""Live regressions for focused sandbox integration scenarios."""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.audit.events import EventType
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.tests._live_config import database_configured
from task_center_runner.tests.mock._focused_scenario_contracts import (
    FocusedScenarioCase,
    assert_focused_scenario_report,
)

pytestmark = pytest.mark.asyncio


_FOCUSED_SANDBOX_CASES: tuple[FocusedScenarioCase, ...] = (
    FocusedScenarioCase(
        "sandbox.occ_concurrent_conflicts",
        min_event_counts={
            EventType.SANDBOX_BATCH_EDIT_APPLIED: 1,
            EventType.SANDBOX_CONFLICT_DETECTED: 1,
            EventType.EXECUTOR_SUCCESS: 1,
        },
        attempt_count=1,
    ),
)


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.parametrize(
    "case",
    _FOCUSED_SANDBOX_CASES,
    ids=[case.name for case in _FOCUSED_SANDBOX_CASES],
)
async def test_focused_sandbox_reference_scenario_runs(
    case: FocusedScenarioCase,
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY[case.name]()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert_focused_scenario_report(report, scenario, case)

"""Live e2e regression: ``CorrectnessTesting`` driven through the generic
``task_center_runner.run_scenario`` (with SWE-EVO sandbox + entry prompt).

This complements ``test_correctness.py`` (which goes through the SWE-EVO
adapter) by exercising the generic entry point directly. Both must produce the
same end-to-end behaviour post-migration.

Skipped when:

- no database URL is configured.
- The Daytona tier-0 health probe fails (no live sandbox available).
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR
from task_center_runner.benchmarks.sweevo.setup import build_sweevo_user_prompt
from task_center_runner import run_scenario
from task_center_runner.audit.events import EventType
from task_center_runner.hooks.builtins import count_events
from task_center_runner.scenarios.correctness_testing import CorrectnessTesting
from task_center_runner.environments.sweevo_image.health import (
    require_sweevo_image_provider_healthy,
)
from task_center_runner.tests._live_config import database_configured


@pytest.mark.asyncio
async def test_correctness_testing_via_live_e2e(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
) -> None:
    if not database_configured():
        pytest.skip("database URL not configured")
    require_sweevo_image_provider_healthy(sweevo_image_instance)

    scenario = CorrectnessTesting()
    extra_hooks = (
        count_events(EventType.PLANNER_INVOKED, name="planner_invocations"),
        count_events(EventType.EVALUATOR_INVOKED, name="evaluator_invocations"),
    )
    report = await run_scenario(
        scenario,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        repo_dir=_REPO_DIR,
        entry_prompt=build_sweevo_user_prompt(sweevo_image_instance, repo_dir=_REPO_DIR),
        extra_hooks=extra_hooks,
        instance_id=sweevo_image_instance.instance_id,
    )

    assert report.task_center_status == "done", (
        f"task_center_status={report.task_center_status!r}: {report.metrics}"
    )
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]

    run_dir = report.run_dir
    assert (run_dir / "run.json").exists()
    run_payload = json.loads((run_dir / "run.json").read_text(encoding="utf-8"))
    assert run_payload["task_center_run_id"] == report.task_center_run_id
    assert run_payload["scenario_name"] == scenario.name

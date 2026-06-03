"""Live regression for the ``sandbox.auto_squash_commit_resume`` scenario.

Drives the OCC mutation critical path past ``AUTO_SQUASH_MAX_DEPTH`` via the
public sandbox toolkit and asserts the contract from
``.omc/plans/occ-layer-stack-commit-resume-auto-squash-report-20260511.md``:

- ``report.request_status == 'done'``.
- ``SANDBOX_LAYER_STACK_LAYERS_SQUASHED``, ``SANDBOX_OCC_CHANGESET_RECEIVED``,
  and ``SANDBOX_OCC_CHANGES_COMMITTED`` appear in both in-memory events and
  ``sandbox_events.jsonl``.
- At least one tool result includes ``layer_stack.auto_squash.total_s``.
- At least one tool result includes ``occ.apply.commit_resume_wait_s``.
- ``layer_stack.auto_squash.depth_before > AUTO_SQUASH_MAX_DEPTH`` appears in
  timing metadata.
- Final ``read_file`` and ``shell`` readback agree on committed contents.
- The intentional missing-anchor edit reports a conflict with non-empty
  ``conflict_reason``, ``is_error == True``, and the same payload shape as the
  synchronous baseline.
- No unexpected tool errors beyond the intentional conflict.
"""

from __future__ import annotations

import json
from collections.abc import Iterable
from pathlib import Path
from typing import Any

import pytest

import sandbox.api as sandbox_api
from test_runner.benchmarks.sweevo.models import SWEEvoInstance
from sandbox.api import ExecCommandRequest, ReadFileRequest, SandboxCaller

from test_runner.audit.events import EventType
from test_runner.scenarios import SCENARIO_REGISTRY
from test_runner.scenarios.sandbox._constants import AUTO_SQUASH_MAX_DEPTH
from test_runner.agent.mock.prompt_inspector import ToolCallRecord
from test_runner.core.stores import TaskStoreBundle
from test_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from test_runner.tests._live_config import database_configured
from test_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    assert_resource_key_max,
    assert_timing_keys_present,
    jsonl_rows,
    load_performance_report,
    mapping,
)
from test_runner.tests.mock._focused_scenario_contracts import count_role_tasks


pytestmark = pytest.mark.asyncio


_REQUIRED_SANDBOX_EVENTS = (
    EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED,
    EventType.SANDBOX_OCC_CHANGESET_RECEIVED,
    EventType.SANDBOX_OCC_CHANGES_COMMITTED,
)
_REQUIRED_PERF_TIMING_KEYS = (
    "layer_stack.auto_squash.total_s",
    "occ.apply.commit_queue_wait_s",
    "occ.apply.commit_resume_wait_s",
    "occ.apply.total_s",
)


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
async def test_auto_squash_commit_resume_crosses_depth_threshold(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
) -> None:
    scenario_cls = SCENARIO_REGISTRY["sandbox.auto_squash_commit_resume"]
    scenario = scenario_cls()
    sandbox_id = str(workspace["sandbox_id"])
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.request_status == "done", report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]
    assert count_role_tasks(report, "executor", status="done") == 5

    seen_events = {event.type for event in report.events}
    missing_events = sorted(
        event.value for event in _REQUIRED_SANDBOX_EVENTS if event not in seen_events
    )
    assert not missing_events, f"missing in-memory events: {missing_events}"

    sandbox_log = report.run_dir / "sandbox_events.jsonl"
    assert sandbox_log.exists()
    runner_event_values = {event.value for event in EventType}
    logged_events = {
        EventType(event_type)
        for row in jsonl_rows(sandbox_log)
        if (event_type := row.get("event_type")) in runner_event_values
    }
    missing_logged = sorted(
        event.value
        for event in _REQUIRED_SANDBOX_EVENTS
        if event not in logged_events
    )
    assert not missing_logged, f"missing persisted events: {missing_logged}"
    assert_o1_workspace_resource_snapshots(sandbox_log)

    timings_records = list(_iter_tool_timings(report.tool_calls))
    assert any(
        "layer_stack.auto_squash.total_s" in timings
        for timings in timings_records
    ), "no tool result reported layer_stack.auto_squash.total_s"
    assert any(
        "occ.apply.commit_resume_wait_s" in timings
        for timings in timings_records
    ), "no tool result reported occ.apply.commit_resume_wait_s"
    assert any(
        float(timings.get("layer_stack.auto_squash.depth_before", 0.0))
        > float(AUTO_SQUASH_MAX_DEPTH)
        for timings in timings_records
    ), (
        "no tool result reported "
        f"layer_stack.auto_squash.depth_before > {AUTO_SQUASH_MAX_DEPTH}"
    )

    intentional_conflict = _find_intentional_conflict(report.tool_calls)
    assert intentional_conflict is not None, "intentional conflict tool call missing"
    assert intentional_conflict.is_error is True
    conflict_metadata = intentional_conflict.metadata
    assert str(conflict_metadata.get("conflict_reason") or ""), (
        "intentional conflict missing conflict_reason: "
        f"{conflict_metadata}"
    )
    assert isinstance(conflict_metadata.get("changed_paths"), list)
    assert isinstance(conflict_metadata.get("status"), str)

    expected_error_count = 1  # only the intentional conflict
    error_calls = [call for call in report.tool_calls if call.is_error]
    assert len(error_calls) == expected_error_count, [
        (call.tool_name, call.metadata.get("status")) for call in error_calls
    ]

    await _assert_final_workspace_state(
        sandbox_id=sandbox_id,
        conflict_status=str(conflict_metadata.get("status") or ""),
        conflict_reason=str(conflict_metadata.get("conflict_reason") or ""),
    )
    await _assert_performance_report(report)


def _iter_tool_timings(
    tool_calls: Iterable[ToolCallRecord],
) -> Iterable[dict[str, Any]]:
    for call in tool_calls:
        timings = call.metadata.get("timings")
        if isinstance(timings, dict):
            yield timings


def _find_intentional_conflict(
    tool_calls: Iterable[ToolCallRecord],
) -> ToolCallRecord | None:
    for call in tool_calls:
        if call.tool_name != "edit_file":
            continue
        if not call.is_error:
            continue
        return call
    return None


async def _assert_performance_report(report: Any) -> None:
    performance_report_task = getattr(report, "performance_report_task", None)
    assert performance_report_task is not None
    perf_path = await performance_report_task
    assert perf_path == report.run_dir / "performance_report.json"
    perf = load_performance_report(report.run_dir)
    assert_timing_keys_present(perf, _REQUIRED_PERF_TIMING_KEYS)
    non_duration = mapping(mapping(perf["sandbox"])["non_duration_observations"])
    depth_before = mapping(non_duration["layer_stack.auto_squash.depth_before"])
    assert float(depth_before["max"]) > float(AUTO_SQUASH_MAX_DEPTH)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_bytes", 0.0)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_exists", 0.0)


async def _assert_final_workspace_state(
    *,
    sandbox_id: str,
    conflict_status: str,
    conflict_reason: str,
) -> None:
    caller = SandboxCaller(agent_id="sweevo-auto-squash-test")
    probe_dir = "/testbed/.ephemeralos/sweevo-mock/auto_squash_commit_resume"
    summary_path = f"{probe_dir}/summary.json"
    summary_read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=summary_path, caller=caller),
    )
    assert summary_read.success
    assert summary_read.exists
    summary_payload = json.loads(summary_read.content)
    assert summary_payload["probe"] == "auto_squash_commit_resume"
    assert summary_payload["write_count"] == AUTO_SQUASH_MAX_DEPTH + 4
    assert summary_payload["conflict_status"] == conflict_status
    assert summary_payload["conflict_reason"] == conflict_reason
    assert summary_payload["conflict_is_error"] is True
    assert float(summary_payload["max_depth_before"]) > float(AUTO_SQUASH_MAX_DEPTH)
    assert float(summary_payload["max_auto_squash_total_s"]) >= 0.0
    assert float(summary_payload["max_commit_resume_wait_s"]) >= 0.0

    edit_target = f"{probe_dir}/edit-target.txt"
    edit_read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=edit_target, caller=caller),
    )
    assert edit_read.success and edit_read.exists
    expected_edit_content = "alpha=new\nbeta=new\n"
    assert edit_read.content == expected_edit_content

    command_result = await sandbox_api.exec_command(
        sandbox_id,
        ExecCommandRequest(
            cmd=f"cat {edit_target}",
            timeout=60,
            caller=caller,
            description="auto-squash commit-resume final exec_command readback",
        ),
    )
    assert command_result.success
    assert command_result.exit_code == 0
    assert command_result.output.stdout.replace("\r\n", "\n") == expected_edit_content

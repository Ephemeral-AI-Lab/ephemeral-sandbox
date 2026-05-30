"""Live regression for the full_stack_adversarial scenario."""

from __future__ import annotations

import hashlib
import json
import os
from collections.abc import Mapping
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.benchmarks.sweevo.setup import select_sweevo_instance
from task_center_runner.benchmarks.sweevo.setup import build_sweevo_user_prompt
from task_center_runner.audit.events import Event, EventType
from task_center_runner.scenarios.full_stack_adversarial import (
    FullStackAdversarial,
)
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from task_center_runner.environments.sweevo_image.health import (
    require_sweevo_image_provider_healthy,
)
from task_center_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    assert_resource_key_max,
    assert_timing_keys_present,
    load_performance_report,
    mapping,
)
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance


_DEFAULT_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_FOREGROUND_SANDBOX_P95_BUDGET_MS = 1_000.0
# shell's tail latency runs high under the loop-driving generator DAG and
# concurrent sandbox load; gate shell on p99 with a generous ceiling so the
# perf budget still catches regressions in the cheap foreground tools.
_SHELL_P99_BUDGET_MS = 15_000.0
_REQUIRED_PERFORMANCE_TOOLS = (
    "shell",
    "read_file",
    "write_file",
    "edit_file",
    "lsp.diagnostics",
    "lsp.hover",
    "lsp.find_definitions",
    "lsp.find_references",
    "lsp.query_symbols",
    "lsp.apply_workspace_edit",
)
_FOREGROUND_SANDBOX_TOOLS = (
    "shell",
    "read_file",
    "write_file",
    "edit_file",
    "lsp.apply_workspace_edit",
)
_REQUIRED_SANDBOX_TIMING_KEYS = (
    "command_exec.mount_workspace_s",
    "command_exec.run_command_s",
    "command_exec.capture_upperdir_s",
    "command_exec.total_s",
    "layer_stack.acquire_snapshot.total_s",
    "occ.commit.total_s",
    "occ.commit.publish_layer_s",
    "occ.apply.total_s",
)
_REQUIRED_TOOL_SAMPLE_TIMINGS = {
    "read_file": ("api.read.total_s", "api.read.layer_stack_read_s"),
    "write_file": ("api.write.total_s", "occ.apply.total_s"),
    "edit_file": ("api.edit.total_s", "occ.apply.total_s"),
    "shell": (
        "command_exec.mount_workspace_s",
        "command_exec.run_command_s",
        "command_exec.capture_upperdir_s",
    ),
    "lsp.apply_workspace_edit": (
        "command_exec.capture_upperdir_s",
        "command_exec.occ_apply_s",
    ),
}
_FORBIDDEN_SANDBOX_EVENT_TEXT = (
    "internal_error",
    "stale lowerdir",
    "manifest references missing layer",
    "missing layer",
    "mount failure",
    "mount_failed",
)


def test_full_stack_instance_fixture_default_contract(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("EOS_SWEEVO_INSTANCE", raising=False)
    instance_id = os.getenv("EOS_SWEEVO_INSTANCE", _DEFAULT_INSTANCE_ID)
    assert select_sweevo_instance(instance_id=instance_id).instance_id == (
        _DEFAULT_INSTANCE_ID
    )


@pytest.mark.asyncio
async def test_full_stack_adversarial_runs_agent_tool_script_matrix(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    require_sweevo_image_provider_healthy(sweevo_image_instance)

    scenario = FullStackAdversarial()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.task_center_status == "done", report.metrics
    assert report.instance_id == _DEFAULT_INSTANCE_ID
    assert report.performance_report_task is not None
    perf_path = await report.performance_report_task
    assert perf_path == report.run_dir / "performance_report.json"

    expected_prompt = build_sweevo_user_prompt(sweevo_image_instance)
    assert report.entry_prompt_length == len(expected_prompt)
    assert report.entry_prompt_sha256 == hashlib.sha256(
        expected_prompt.encode("utf-8")
    ).hexdigest()

    assert len(report.requirement_ledger) > 30  # dask renders ~39 requirements
    assert len(report.package_plan) >= 4
    assert len(report.matrix_plan) >= 32

    _assert_task_center_shape(report.graph_summary)
    _assert_message_logs(report.run_dir)
    _assert_sandbox_monitor_events(report.events, report.run_dir)
    _assert_full_stack_performance_report_complete(report.run_dir)
    await _assert_final_sandbox_state(
        sandbox_id=report.sandbox_id,
        task_center_run_id=report.task_center_run_id,
    )


def _assert_task_center_shape(graph_summary: dict[str, Any]) -> None:
    assert _has_deferred_attempt(graph_summary)
    assert _has_closing_passed_attempt(graph_summary)
    assert _count_failed_verifier_tasks(graph_summary) >= 1
    assert _has_multi_dependency_verifier(graph_summary)
    assert _recursive_workflow_count(graph_summary) >= 1
    assert _recursive_workflows_succeeded(graph_summary), graph_summary
    assert _verifier_task_done_with_checkpoint(graph_summary, "recursive_return")
    assert _verifier_task_done_with_checkpoint(graph_summary, "final_release")


def _has_deferred_attempt(graph_summary: dict[str, Any]) -> bool:
    return any(
        attempt["deferred_goal_for_next_iteration"]
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
    )


def _has_closing_passed_attempt(graph_summary: dict[str, Any]) -> bool:
    return any(
        attempt["status"] == "passed"
        and not attempt["deferred_goal_for_next_iteration"]
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
    )


def _count_failed_verifier_tasks(graph_summary: dict[str, Any]) -> int:
    return sum(
        1
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
        if task.get("agent_name") == "verifier" and task.get("status") == "failed"
    )


def _has_multi_dependency_verifier(graph_summary: dict[str, Any]) -> bool:
    for workflow in graph_summary["workflows"]:
        for iteration in workflow["iterations"]:
            for attempt in iteration["attempts"]:
                for task in attempt["tasks"]:
                    if task.get("agent_name") == "verifier" and len(task["needs"]) > 1:
                        return True
    return False


def _recursive_workflow_count(graph_summary: dict[str, Any]) -> int:
    return sum(
        1
        for workflow in graph_summary["workflows"]
        if workflow.get("origin_kind") == "task"
    )


def _recursive_workflows_succeeded(graph_summary: dict[str, Any]) -> bool:
    recursive = [
        workflow
        for workflow in graph_summary["workflows"]
        if workflow.get("origin_kind") == "task"
    ]
    return bool(recursive) and all(
        workflow.get("status") == "succeeded" for workflow in recursive
    )


def _verifier_task_done_with_checkpoint(
    graph_summary: dict[str, Any],
    checkpoint: str,
) -> bool:
    needle = f"checkpoint={checkpoint}"
    return any(
        task.get("agent_name") == "verifier"
        and task.get("status") == "done"
        and needle in str(task.get("context_message") or "")
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
    )


def _assert_message_logs(run_dir: Path) -> None:
    messages = _message_rows(run_dir)
    assert messages, f"no message.jsonl agent messages under {run_dir}"
    agents = {
        str((message.get("metadata") or {}).get("agent_name") or "")
        for message in messages
        if isinstance(message.get("metadata"), dict)
    }
    assert {
        "planner",
        "verifier",
        "evaluator",
    } <= agents
    assert any(_is_executor_agent_name(agent) for agent in agents)
    tool_uses = {
        str(block.get("name") or "")
        for message in messages
        for block in message.get("content", [])
        if isinstance(block, dict) and block.get("type") == "tool_use"
    }
    assert {
        "write_file",
        "edit_file",
        "read_file",
        "shell",
        "lsp.hover",
        "lsp.find_definitions",
        "lsp.find_references",
        "lsp.diagnostics",
        "lsp.query_symbols",
        "lsp.apply_workspace_edit",
    } <= tool_uses
    assert any(
        block.get("type") == "tool_result"
        and (message.get("metadata") or {}).get("tool_name") == "edit_file"
        and (message.get("metadata") or {}).get("is_error")
        for message in messages
        for block in message.get("content", [])
        if isinstance(block, dict) and isinstance(message.get("metadata"), dict)
    )


def _assert_sandbox_monitor_events(events: list[Event], run_dir: Path) -> None:
    required = {
        EventType.SANDBOX_LAYER_STACK_LEASE_ACQUIRED,
        EventType.SANDBOX_LAYER_STACK_LAYER_CREATED,
        EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED,
        EventType.SANDBOX_OVERLAY_EXECUTED,
        EventType.SANDBOX_OCC_CHANGESET_RECEIVED,
        EventType.SANDBOX_OCC_CHANGES_COMMITTED,
        EventType.SANDBOX_CONFLICT_DETECTED,
    }
    seen = {event.type for event in events}
    missing = sorted(event.value for event in required - seen)
    assert not missing, f"missing sandbox monitor events: {missing}"

    sandbox_log = run_dir / "sandbox_events.jsonl"
    assert sandbox_log.exists()
    runner_event_values = {event.value for event in EventType}
    logged = {
        EventType(event_type)
        for row in _jsonl_rows(sandbox_log)
        if (event_type := row.get("event_type")) in runner_event_values
    }
    missing_logged = sorted(event.value for event in required - logged)
    assert not missing_logged, f"missing persisted sandbox events: {missing_logged}"


def _assert_full_stack_performance_report_complete(run_dir: Path) -> None:
    perf = load_performance_report(run_dir)
    per_tool = mapping(mapping(perf["tools"])["per_tool"])
    for tool_name in _REQUIRED_PERFORMANCE_TOOLS:
        _assert_tool_latency_stats(per_tool, tool_name)
    for tool_name in _FOREGROUND_SANDBOX_TOOLS:
        _assert_foreground_tool_latency(per_tool, tool_name)
    for tool_name, timing_keys in _REQUIRED_TOOL_SAMPLE_TIMINGS.items():
        _assert_tool_samples_include_timings(per_tool, tool_name, timing_keys)

    assert_timing_keys_present(perf, _REQUIRED_SANDBOX_TIMING_KEYS)
    assert_o1_workspace_resource_snapshots(run_dir / "sandbox_events.jsonl")
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_bytes", 0.0)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_exists", 0.0)
    _assert_cgroup_metrics_are_run_deltas(perf)
    _assert_no_forbidden_sandbox_event_text(run_dir / "sandbox_events.jsonl")
    _assert_recursive_workflow_keeps_sandbox_responsive(run_dir, per_tool)


def _assert_tool_latency_stats(
    per_tool: Mapping[str, Any],
    tool_name: str,
) -> None:
    assert tool_name in per_tool, f"missing performance samples for {tool_name}"
    stats = mapping(per_tool[tool_name])
    assert int(stats.get("count") or 0) > 0, stats
    for key in ("p50_ms", "p95_ms", "max_ms"):
        assert key in stats, f"{tool_name} missing {key}"
        assert float(stats[key]) >= 0.0, f"{tool_name} {key}={stats[key]}"


def _assert_tool_percentile_under(
    per_tool: Mapping[str, Any],
    tool_name: str,
    budget_ms: float,
    *,
    stat_key: str = "p95_ms",
) -> None:
    value_ms = float(mapping(per_tool[tool_name]).get(stat_key) or 0.0)
    assert value_ms <= budget_ms, (
        f"{tool_name} {stat_key} {value_ms:.3f}ms exceeds {budget_ms:.0f}ms"
    )


def _assert_foreground_tool_latency(
    per_tool: Mapping[str, Any],
    tool_name: str,
) -> None:
    # shell rides the high-latency tail (p99 ceiling); the cheap foreground
    # tools keep the tight p95 budget.
    if tool_name == "shell":
        _assert_tool_percentile_under(
            per_tool, tool_name, _SHELL_P99_BUDGET_MS, stat_key="p99_ms"
        )
    else:
        _assert_tool_percentile_under(
            per_tool, tool_name, _FOREGROUND_SANDBOX_P95_BUDGET_MS
        )


def _assert_tool_samples_include_timings(
    per_tool: Mapping[str, Any],
    tool_name: str,
    timing_keys: tuple[str, ...],
) -> None:
    samples = list(mapping(per_tool[tool_name]).get("samples") or ())
    missing = [
        key
        for key in timing_keys
        if not any(key in mapping(sample).get("timings_s", {}) for sample in samples)
    ]
    assert not missing, f"{tool_name} samples missing timing keys: {missing}"


def _assert_cgroup_metrics_are_run_deltas(perf: Mapping[str, Any]) -> None:
    resources = mapping(mapping(perf["sandbox"])["resource_keys"])
    for key in (
        "resource.cgroup.cpu_usage_usec",
        "resource.cgroup.io_wbytes",
    ):
        assert key in resources, f"missing cgroup resource key: {key}"
        stats = mapping(resources[key])
        assert stats.get("source") == "run_delta", stats
        assert float(stats.get("latest") or 0.0) <= float(
            stats.get("latest_lifetime") or 0.0
        )
        assert float(stats.get("first_lifetime") or 0.0) <= float(
            stats.get("latest_lifetime") or 0.0
        )


def _assert_no_forbidden_sandbox_event_text(events_path: Path) -> None:
    raw = events_path.read_text(encoding="utf-8", errors="replace").lower()
    for needle in _FORBIDDEN_SANDBOX_EVENT_TEXT:
        assert needle not in raw, f"{needle!r} appears in {events_path}"


def _assert_recursive_workflow_keeps_sandbox_responsive(
    run_dir: Path,
    per_tool: Mapping[str, Any],
) -> None:
    messages = _message_rows(run_dir)
    recursive_tool_uses = _tool_uses_for_task(messages, "recursive_")
    assert {"read_file", "write_file"} <= recursive_tool_uses
    final_reconciliation_tool_uses = _tool_uses_for_task(
        messages, "final_reconciliation"
    )
    assert {"read_file", "shell"} <= final_reconciliation_tool_uses
    for tool_name in _FOREGROUND_SANDBOX_TOOLS:
        _assert_foreground_tool_latency(per_tool, tool_name)


def _tool_uses_for_task(
    messages: list[dict[str, Any]],
    task_id_part: str,
) -> set[str]:
    return {
        str(block.get("name") or "")
        for message in messages
        if task_id_part in str((message.get("metadata") or {}).get("task_id") or "")
        for block in message.get("content", [])
        if isinstance(block, dict) and block.get("type") == "tool_use"
    }


async def _assert_final_sandbox_state(
    *,
    sandbox_id: str,
    task_center_run_id: str,
) -> None:
    import sandbox.api as sandbox_api
    from sandbox.api import ReadFileRequest, SandboxCaller, ShellRequest

    caller = SandboxCaller(agent_id="sweevo-full-stack-test")
    final_path = "/testbed/.ephemeralos/sweevo-mock/full_stack/final-reconciliation.json"
    final = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=final_path, caller=caller),
    )
    assert final.success and final.exists
    final_payload = json.loads(final.content)
    assert final_payload["scenario"] == "full_stack_adversarial"
    assert final_payload["failed_cells"] == 0
    assert final_payload["recursive_workflows"] == 1
    assert final_payload["manifest_end"] > final_payload["manifest_start"]

    lsp_path = "/testbed/.ephemeralos/sweevo-mock/full_stack/lsp-matrix.json"
    lsp = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=lsp_path, caller=caller),
    )
    assert lsp.success and lsp.exists
    assert json.loads(lsp.content)["subsystem"] == "lsp"

    metrics_path = (
        "/testbed/.omc/results/"
        f"full-stack-adversarial-{_safe_slug(task_center_run_id)}.jsonl"
    )
    metrics = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=metrics_path, caller=caller),
    )
    assert metrics.success and metrics.exists
    rows = [json.loads(line) for line in metrics.content.splitlines() if line.strip()]
    summary_rows = [
        row
        for row in rows
        if row.get("schema") == "full_stack_adversarial.summary.v1"
    ]
    assert summary_rows
    summary = summary_rows[-1]
    assert summary["failed_cells"] == 0
    assert summary["passed_cells"] >= 32
    assert summary["expected_tool_errors"] >= 1
    assert summary["conflicts_detected"] >= 1
    assert any(row.get("subsystem") == "lsp" for row in rows)

    shell = await sandbox_api.shell(
        sandbox_id,
        ShellRequest(
            command=(
                f"test -s {final_path} && test -d /testbed/.git && "
                "printf 'workspace=/testbed\\n'"
            ),
            cwd="/testbed",
            timeout=60,
            caller=caller,
            description="verify final full-stack sandbox state",
        ),
    )
    assert shell.success
    assert shell.exit_code == 0
    assert "workspace=/testbed" in shell.stdout


def _message_rows(run_dir: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    message_paths = list(run_dir.rglob("message.jsonl"))
    assert message_paths, f"no message.jsonl files under {run_dir}"
    for path in message_paths:
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                rows.append(json.loads(line))
    return rows


def _is_executor_agent_name(agent_name: str) -> bool:
    return agent_name == "executor" or agent_name.startswith("executor_")


def _jsonl_rows(path: Path) -> list[dict[str, Any]]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _safe_slug(value: str) -> str:
    return "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in value)

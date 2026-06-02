"""Live regression for the dynamic full_case_user_input scenario."""

from __future__ import annotations

import hashlib
import json
import os
import uuid
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from runtime.app_factory import model_store
from test_runner.benchmarks.sweevo.setup import select_sweevo_instance
from test_runner.benchmarks.sweevo.setup import build_sweevo_user_prompt
from test_runner.audit.events import EventType
from test_runner.scenarios.full_case_user_input import (
    FullCaseUserInput,
)
from test_runner.core.stores import TaskStoreBundle
from test_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from test_runner.environments.sweevo_image.health import (
    require_sweevo_image_provider_healthy,
)
from test_runner.tests._live_config import rust_sandbox_runtime_unavailable_reason
from test_runner.tests.mock._focused_scenario_contracts import recursive_workflows
from test_runner.benchmarks.sweevo.models import SWEEvoInstance


_DEFAULT_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_RUST_RUNTIME_UNAVAILABLE = rust_sandbox_runtime_unavailable_reason()


@pytest.fixture
def _active_mock_model(stores: TaskStoreBundle) -> Iterator[None]:
    prior_sf = model_store._session_factory  # noqa: SLF001
    model_store.initialize(stores.session_factory)
    key = f"test/mock-loop-{uuid.uuid4().hex[:8]}"
    model_store.register(
        key=key,
        label="Mock Loop Runner",
        class_path="providers.clients.anthropic_native:AnthropicClient",
        kwargs={"model": "mock-loop", "max_tokens": 4096},
        activate=True,
    )
    try:
        yield
    finally:
        try:
            model_store.delete(key)
        except Exception:
            pass
        model_store._session_factory = prior_sf  # noqa: SLF001


def test_sweevo_instance_fixture_default_contract(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EOS_SWEEVO_INSTANCE", raising=False)
    instance_id = os.getenv("EOS_SWEEVO_INSTANCE", _DEFAULT_INSTANCE_ID)
    assert select_sweevo_instance(instance_id=instance_id).instance_id == (
        _DEFAULT_INSTANCE_ID
    )


@pytest.mark.asyncio
@pytest.mark.skipif(
    _RUST_RUNTIME_UNAVAILABLE is not None,
    reason=_RUST_RUNTIME_UNAVAILABLE or "Rust sandbox runtime unavailable",
)
async def test_full_case_user_input_runs_dynamic_verifier_dag(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
    _active_mock_model: None,
) -> None:
    require_sweevo_image_provider_healthy(sweevo_image_instance)

    scenario = FullCaseUserInput()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.request_status == "done", report.metrics
    assert report.instance_id == sweevo_image_instance.instance_id
    assert report.instance_id == _DEFAULT_INSTANCE_ID

    expected_prompt = build_sweevo_user_prompt(sweevo_image_instance)
    assert report.entry_prompt_length == len(expected_prompt)
    assert report.entry_prompt_sha256 == hashlib.sha256(
        expected_prompt.encode("utf-8")
    ).hexdigest()

    run_payload = json.loads(
        (report.run_dir / "run.json").read_text(encoding="utf-8")
    )
    assert run_payload["instance_id"] == _DEFAULT_INSTANCE_ID

    assert len(report.requirement_ledger) > 30  # dask renders ~39 requirements
    executor_count = sum(1 for launch in report.launches if launch.role == "executor")
    reducer_count = sum(1 for launch in report.launches if launch.role == "reducer")
    # Guard tasks are executor generators carrying a ``VERIFY checkpoint=`` spec;
    # the reducer is the per-attempt gate. Each attempt runs exactly one reducer,
    # so reducers stay well below the executor (worker + guard) count.
    assert executor_count >= 12
    assert reducer_count >= 4
    assert reducer_count < executor_count

    # --- lifecycle migrated to graph_summary (event-source runner emits no
    # lifecycle events; assert the protected outcome via real store state) ----
    gs = report.graph_summary
    # At least one attempt carried a deferral.
    assert _attempt_deferred(gs), gs
    assert _continuation_iterations_follow_partial_attempts(gs)
    # Guard density: the dynamic DAG wires several VERIFY-checkpoint guard tasks
    # (one per executor wave plus the recursive/final guards).
    assert _count_guard_tasks(gs) >= 4, gs
    assert _has_multi_dependency_guard(gs)
    # At least one reducer gate failed (the only failed task in a failing attempt).
    assert _count_failed_reducer_tasks(gs) >= 1, gs
    planner_inspections = [
        item for item in report.prompt_inspections if item.role == "planner"
    ]
    assert any(
        item.checks.get("failed_attempts")
        or item.checks.get("previous_iteration_results")
        for item in planner_inspections
    ), planner_inspections

    # A delegated (task-origin) workflow exists and succeeded. Checkpoint-gated
    # ordering is now enforced by task/request dependency and closure state.
    recursive = recursive_workflows(gs)
    assert recursive, gs
    assert all(workflow["status"] == "succeeded" for workflow in recursive), recursive
    # recursive child closed before the parent's recursive_return guard passed.
    assert _guard_task_done_with_checkpoint(gs, "recursive_return"), gs
    # final release: the entry workflow's final attempt passed and its
    # final_release guard is done (it gates the reducer).
    entry = _entry_workflow(gs)
    final_attempt = entry["iterations"][-1]["attempts"][-1]
    assert final_attempt["status"] == "passed", final_attempt
    assert _guard_task_done_with_checkpoint(gs, "final_release"), gs

    _assert_audit_tree_roles(report.run_dir)
    _assert_message_jsonl_contains_tool_scripts(report.run_dir)
    _assert_parallel_task_wave(gs)
    _assert_sandbox_monitor_events(report)
    await _assert_daytona_workspace_tool_state(report.sandbox_id)


def _continuation_iterations_follow_partial_attempts(
    graph_summary: dict[str, Any],
) -> bool:
    for workflow in graph_summary["workflows"]:
        iterations = workflow["iterations"]
        by_sequence = {iteration["sequence_no"]: iteration for iteration in iterations}
        for iteration in iterations:
            if iteration["sequence_no"] <= 1:
                continue
            previous = by_sequence[iteration["sequence_no"] - 1]
            final_attempt = previous["attempts"][-1]
            if not final_attempt["deferred_goal_for_next_iteration"]:
                return False
    return True


def _is_guard_task(task: dict[str, Any]) -> bool:
    """A guard is an executor generator whose spec is a ``VERIFY checkpoint=`` line.

    Guard tasks were ``verifier`` agents in the old model; they are now plain
    executor generators gated by the reducer, identified by their preserved
    ``VERIFY checkpoint=`` ``instruction`` spec.
    """
    return task.get("agent_name") == "executor" and "VERIFY checkpoint=" in str(
        task.get("instruction") or ""
    )


def _count_guard_tasks(graph_summary: dict[str, Any]) -> int:
    return sum(
        1
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
        if _is_guard_task(task)
    )


def _has_multi_dependency_guard(graph_summary: dict[str, Any]) -> bool:
    for workflow in graph_summary["workflows"]:
        for iteration in workflow["iterations"]:
            for attempt in iteration["attempts"]:
                for task in attempt["tasks"]:
                    if _is_guard_task(task) and len(task["needs"]) > 1:
                        return True
    return False


def _entry_workflow(graph_summary: dict[str, Any]) -> dict[str, Any]:
    for workflow in graph_summary["workflows"]:
        if str(workflow.get("parent_task_id") or "").startswith("root-"):
            return workflow
    raise AssertionError(f"no entry workflow in graph_summary: {graph_summary}")


def _attempt_deferred(graph_summary: dict[str, Any]) -> bool:
    return any(
        attempt["deferred_goal_for_next_iteration"]
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
    )


def _count_failed_reducer_tasks(graph_summary: dict[str, Any]) -> int:
    """Count failed reducer-gate tasks across all attempts.

    A failing attempt now fails at its reducer gate (its generators all
    succeed); the failed task is identified by membership in the attempt's
    ``reducer_task_ids``.
    """
    return sum(
        1
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
        if task.get("task_id") in set(attempt["reducer_task_ids"])
        and task.get("status") == "failed"
    )


def _guard_task_done_with_checkpoint(
    graph_summary: dict[str, Any],
    checkpoint: str,
) -> bool:
    """A guard executor task whose spec carries ``checkpoint=<checkpoint>`` is done.

    The guard task's ``instruction`` is its ``VERIFY checkpoint=<x> ...``
    spec; the former checkpoint-gated event ordering is replaced by asserting
    that the corresponding guard task reached ``done`` (task/request's own
    dependency enforcement guarantees it ran after its upstream tasks closed).
    """
    needle = f"checkpoint={checkpoint}"
    return any(
        _is_guard_task(task)
        and task.get("status") == "done"
        and needle in str(task.get("instruction") or "")
        for workflow in graph_summary["workflows"]
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
    )


def _assert_audit_tree_roles(run_dir: Path) -> None:
    assert (run_dir / "run.json").exists()
    role_segments: set[str] = set()
    for role_dir in run_dir.rglob("[0-9][0-9]_*_*"):
        role_segments.add(role_dir.name.split("_", 2)[1])
        assert (role_dir / "task.json").exists()
    assert {"executor", "reducer"}.issubset(role_segments)
    workflow_dirs = sorted(run_dir.glob("workflow_*_*"))
    assert workflow_dirs
    assert list(run_dir.glob("workflow_*_*/iteration_*_*"))
    assert list(run_dir.glob("workflow_*_*/iteration_*_*/attempt_*_*"))
    first_workflow = workflow_dirs[0]
    workflow = _json_file(first_workflow / "workflow.json")
    # The entry workflow's parent is the root Task created for the request.
    assert str(workflow["parent_task_id"]).startswith("root-")
    iteration_files = sorted(first_workflow.glob("iteration_*_*/iteration.json"))
    assert iteration_files
    first_iteration = _json_file(iteration_files[0])
    assert first_iteration["attempt_ids"], "first goal must be delegated work"


def _assert_message_jsonl_contains_tool_scripts(run_dir: Path) -> None:
    messages = _message_steps(run_dir)
    assert messages, f"no message.jsonl agent messages under {run_dir}"
    assert all(
        "role" in message and "content" in message for message in messages
    )
    assert all("step_type" not in message for message in messages)
    agents = {
        str((message.get("metadata") or {}).get("agent_name") or "")
        for message in messages
        if isinstance(message.get("metadata"), dict)
    }
    assert any(_is_executor_agent_name(agent) for agent in agents)
    assert "reducer" in agents
    tool_calls = {
        str(block.get("name") or "")
        for message in messages
        for block in message.get("content", [])
        if isinstance(block, dict) and block.get("type") == "tool_use"
    }
    assert {"write_file", "edit_file", "read_file", "shell"}.issubset(tool_calls)
    assert "system" in {str(message.get("role") or "") for message in messages}
    assert "user" in {str(message.get("role") or "") for message in messages}
    assert "assistant" in {
        str(message.get("role") or "") for message in messages
    }
    assert any(
        block.get("type") == "tool_result"
        and (message.get("metadata") or {}).get("tool_name") == "write_file"
        and not (message.get("metadata") or {}).get("is_error")
        for message in messages
        for block in message.get("content", [])
        if isinstance(block, dict) and isinstance(message.get("metadata"), dict)
    )
    assert any(
        block.get("type") == "tool_result"
        and (message.get("metadata") or {}).get("tool_name") == "edit_file"
        and (message.get("metadata") or {}).get("is_error")
        for message in messages
        for block in message.get("content", [])
        if isinstance(block, dict) and isinstance(message.get("metadata"), dict)
    )


def _assert_parallel_task_wave(graph_summary: dict[str, Any]) -> None:
    for workflow in graph_summary["workflows"]:
        for iteration in workflow["iterations"]:
            for attempt in iteration["attempts"]:
                tasks_by_deps: dict[tuple[str, ...], list[dict[str, Any]]] = {}
                for task in attempt["tasks"]:
                    if (
                        task.get("agent_name") != "executor"
                        or task.get("status") != "done"
                    ):
                        continue
                    deps = tuple(str(dep) for dep in task.get("needs") or ())
                    tasks_by_deps.setdefault(deps, []).append(task)
                if any(len(tasks) >= 2 for tasks in tasks_by_deps.values()):
                    return
    raise AssertionError("no parallel executor wave in graph_summary")


def _assert_sandbox_monitor_events(report: Any) -> None:
    required = {
        EventType.SANDBOX_LAYER_STACK_LEASE_ACQUIRED,
        EventType.SANDBOX_LAYER_STACK_LAYER_CREATED,
        EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED,
        EventType.SANDBOX_OVERLAY_EXECUTED,
        EventType.SANDBOX_OCC_CHANGESET_RECEIVED,
        EventType.SANDBOX_OCC_CHANGES_COMMITTED,
        EventType.SANDBOX_CONFLICT_DETECTED,
    }
    seen = {event.type for event in report.events}
    assert required <= seen
    assert int(report.metrics.get("tool_errors_total") or 0) >= 1

    event_log = report.run_dir / "sandbox_events.jsonl"
    assert event_log.exists()
    rows = _jsonl_rows(event_log)
    runner_event_values = {event.value for event in EventType}
    logged = {
        EventType(event_type)
        for row in rows
        if (event_type := row.get("event_type")) in runner_event_values
    }
    assert required <= logged


async def _assert_daytona_workspace_tool_state(sandbox_id: str) -> None:
    import sandbox.api as sandbox_api
    from sandbox.api import ExecCommandRequest, ReadFileRequest, SandboxCaller
    from sandbox.host.daemon_client import call_daemon_api

    caller = SandboxCaller(agent_id="sweevo-live-test")
    binding_payload = await call_daemon_api(
        sandbox_id,
        "api.workspace_binding",
        {"agent_id": caller.agent_id},
        timeout=30,
    )
    binding = binding_payload.get("binding")
    assert isinstance(binding, dict), binding_payload
    assert binding.get("workspace_root") == "/testbed"
    assert int(binding.get("base_manifest_version") or 0) >= 1

    proof_path = "/testbed/.ephemeralos/sweevo-mock/full_case/workspace-proof.txt"
    proof = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(path=proof_path, caller=caller),
    )
    assert proof.success
    assert proof.exists
    assert "declared_workspace=/testbed" in proof.content

    command = await sandbox_api.exec_command(
        sandbox_id,
        ExecCommandRequest(
            cmd=f"test -s {proof_path} && printf 'workspace=/testbed\\n'",
            timeout=60,
            caller=caller,
            description="verify SWE-EVO tool state in /testbed",
        ),
    )
    assert command.success
    assert command.exit_code == 0
    assert "workspace=/testbed" in command.output.stdout


def _jsonl_rows(path: Path) -> list[dict[str, Any]]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _json_file(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _message_steps(run_dir: Path) -> list[dict[str, Any]]:
    steps: list[dict[str, Any]] = []
    message_paths = list(run_dir.rglob("message.jsonl"))
    assert message_paths, f"no message.jsonl files under {run_dir}"
    for path in message_paths:
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                steps.append(json.loads(line))
    return steps


def _is_executor_agent_name(agent_name: str) -> bool:
    return agent_name == "executor" or agent_name.startswith("executor_")

"""TaskCenter entry bootstrap tests."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from task_center import start_task_center_run
from task_center.entry import TaskCenterSandboxProvisioner
from task_center.workflow.state import WorkflowOriginKind
from task_center._core.task_state import TaskCenterTaskRole, SpawnReason


@pytest.mark.asyncio
async def test_entry_bootstrap_converts_prompt_to_initial_workflow(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    context_packet_store,
    register_test_agents,
) -> None:
    release_runner = None
    asyncio_event: asyncio.Event | None = None

    async def runner(*args, **kwargs):  # type: ignore[no-untyped-def]
        nonlocal release_runner, asyncio_event

        if asyncio_event is None:
            asyncio_event = asyncio.Event()
            release_runner = asyncio_event.set
        await asyncio_event.wait()
        return SimpleNamespace(status="ok")

    runtime_cfg = SimpleNamespace(cwd="/tmp")
    handle = start_task_center_run(
        config=runtime_cfg,
        prompt="solve the user request",
        sandbox_id=None,
        on_agent_event=None,
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        context_packet_store=context_packet_store,
        runner=runner,
        sandbox_provisioner=TaskCenterSandboxProvisioner(
            create_fn=lambda **_kwargs: {"id": "sandbox-entry-test"}
        ),
    )
    await asyncio.sleep(0)

    workflow = workflow_store.get(handle.workflow_id)
    iteration = iteration_store.get(handle.initial_iteration_id)
    attempt = attempt_store.get(handle.initial_attempt_id)
    planner_task = task_store.get_task(f"{handle.initial_attempt_id}:planner")
    run_tasks = task_store.list_tasks_for_run(handle.task_center_run_id)

    assert workflow is not None
    assert workflow.origin_kind == WorkflowOriginKind.ENTRY
    assert workflow.requested_by_task_id is None
    assert workflow.goal == "solve the user request"
    assert iteration is not None
    assert iteration.goal == "solve the user request"
    assert attempt is not None
    assert planner_task is not None
    assert planner_task["role"] == TaskCenterTaskRole.PLANNER.value
    assert planner_task["spawn_reason"] == SpawnReason.ATTEMPT_PLANNER.value
    assert [task["role"] for task in run_tasks] == [TaskCenterTaskRole.PLANNER.value]

    assert release_runner is not None
    release_runner()
    await handle.launcher.wait_for_idle()

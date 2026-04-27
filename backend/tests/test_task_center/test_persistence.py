"""Persistence tests for TaskCenter request/run/task/harness-graph records."""

from __future__ import annotations

from collections.abc import Awaitable, Callable

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

import db.models  # noqa: F401
from db.base import Base
from db.stores.agent_run_store import AgentRunStore
from db.stores.task_center_store import TaskCenterStore
from task_center.runtime import TaskCenter


Action = Callable[[TaskCenter, str], Awaitable[None]]


def _memory_store() -> tuple[TaskCenterStore, AgentRunStore]:
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    task_center_store = TaskCenterStore()
    task_center_store.initialize(sf)
    agent_run_store = AgentRunStore()
    agent_run_store.initialize(sf)
    return task_center_store, agent_run_store


def _scripted_spawn(scripts: dict[str, Action]):
    async def spawn(task_id: str, tc: TaskCenter, sandbox_id: str | None) -> None:
        del sandbox_id
        action = scripts.get(task_id)
        if action is not None:
            await action(tc, task_id)

    return spawn


@pytest.mark.asyncio
async def test_task_center_persists_request_run_tasks_and_harness_graph() -> None:
    store, _ = _memory_store()
    store.create_request(
        request_id="req1",
        cwd="/repo",
        sandbox_id="sandbox-1",
        request_prompt="do the work",
    )
    store.create_run(run_id="run1", request_id="req1")

    async def root_action(tc, tid):
        tc.request_plan(tid, "decompose")

    async def planner_action(tc, tid):
        tc.submit_plan_handoff(tid, [{"id": "child"}], {"child": "child input"}, "h", "evaluate")

    async def child_action(tc, tid):
        tc.submit_task_success(tid, "child done")

    async def eval_action(tc, tid):
        tc.submit_task_success(tid, "accepted")

    tc = TaskCenter(
        spawn_func=_scripted_spawn(
            {
                "t1": root_action,
                "t2": planner_action,
                "child": child_action,
                "t2-eval": eval_action,
            }
        ),
        request_id="req1",
        run_id="run1",
        task_center_store=store,
    )
    root = await tc.run_query("do the work", sandbox_id="sandbox-1")

    assert root.status.value == "done"

    request = store.get_request("req1")
    assert request is not None
    assert request.request_prompt == "do the work"

    runs = store.list_runs_for_request("req1")
    assert runs[0]["status"] == "done"
    assert runs[0]["root_task_id"] == "run1:t1"

    tasks = {task["id"]: task for task in store.list_tasks_for_run("run1")}
    assert tasks["run1:t1"]["status"] == "done"
    assert tasks["run1:t1"]["task_input"] == "do the work"
    assert tasks["run1:t1"]["task_center_harness_graph_id"] is None
    assert any(s["kind"] == "child_success" for s in tasks["run1:t1"]["summaries"])
    assert tasks["run1:t2"]["role"] == "planner"
    assert tasks["run1:child"]["task_input"] == "child input"
    assert any(s["kind"] == "success" for s in tasks["run1:child"]["summaries"])
    assert tasks["run1:t2-eval"]["role"] == "evaluator"

    harness_graphs = store.list_harness_graphs_for_run("run1")
    assert len(harness_graphs) == 1
    g = harness_graphs[0]
    assert g["root_task_id"] == "run1:t1"
    assert g["planner_task_id"] == "run1:t2"
    assert g["evaluator_task_id"] == "run1:t2-eval"
    assert g["executor_task_ids"] == ["run1:child"]


def test_agent_run_is_one_to_one_with_task() -> None:
    store, agent_runs = _memory_store()
    store.create_request(
        request_id="req1",
        cwd="/repo",
        sandbox_id=None,
        request_prompt="prompt",
    )
    store.create_run(run_id="run1", request_id="req1")
    store.upsert_task(
        task_id="run1:t1",
        run_id="run1",
        role="executor",
        task_input="prompt",
        status="running",
        summaries=[],
        needs=[],
        task_center_harness_graph_id=None,
    )

    agent_runs.create_run(
        run_id="agent1",
        task_id="run1:t1",
        agent_name="executor",
    )
    agent_runs.finish_run(
        "agent1",
        message_history=[{"role": "user", "content": "prompt"}],
        terminal_tool_result={"output": "done"},
        token_count=7,
    )

    record = agent_runs.get_run("agent1")
    assert record is not None
    assert record.task_id == "run1:t1"
    assert record.terminal_tool_result == {"output": "done"}
    assert record.token_count == 7

"""Snapshot tests for persisted TaskCenter task and harness-graph topology."""

from __future__ import annotations

from collections.abc import Awaitable, Callable

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

import db.models  # noqa: F401
from db.base import Base
from db.stores.task_center_store import TaskCenterStore
from task_center.runtime import TaskCenter


Action = Callable[[TaskCenter, str], Awaitable[None]]


def _memory_store() -> TaskCenterStore:
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    store = TaskCenterStore()
    store.initialize(sf)
    return store


def _create_run(store: TaskCenterStore, *, request_id: str, run_id: str, prompt: str) -> None:
    store.create_request(
        request_id=request_id, cwd="/repo", sandbox_id=None, request_prompt=prompt
    )
    store.create_run(run_id=run_id, request_id=request_id)


def _scripted_spawn(scripts: dict[str, Action]):
    async def spawn(task_id: str, tc: TaskCenter, sandbox_id: str | None) -> None:
        del sandbox_id
        action = scripts.get(task_id)
        if action is not None:
            await action(tc, task_id)

    return spawn


@pytest.mark.asyncio
async def test_single_task_persists_no_harness_graph() -> None:
    store = _memory_store()
    _create_run(store, request_id="req", run_id="run", prompt="finish directly")

    async def root_action(tc, tid):
        tc.submit_task_success(tid, "root done")

    tc = TaskCenter(
        spawn_func=_scripted_spawn({"t1": root_action}),
        request_id="req",
        run_id="run",
        task_center_store=store,
    )
    await tc.run_query("finish directly")

    tasks = {t["id"]: t for t in store.list_tasks_for_run("run")}
    assert tasks["run:t1"]["status"] == "done"
    assert tasks["run:t1"]["task_center_harness_graph_id"] is None
    assert store.list_harness_graphs_for_run("run") == []


@pytest.mark.asyncio
async def test_plan_handoff_persists_harness_graph_with_planner_executors_and_evaluator() -> None:
    store = _memory_store()
    _create_run(store, request_id="req", run_id="run", prompt="plan it")

    async def root_action(tc, tid):
        tc.request_plan(tid, "decompose")

    async def planner_action(tc, tid):
        tc.submit_plan_handoff(
            tid,
            [{"id": "left"}, {"id": "right", "deps": ["left"]}],
            {"left": "do left", "right": "do right"},
            "left then right",
            "evaluate",
        )

    async def child_action(tc, tid):
        tc.submit_task_success(tid, f"{tid} done")

    async def eval_action(tc, tid):
        tc.submit_task_success(tid, "accepted")

    scripts = {
        "t1": root_action,
        "t2": planner_action,
        "left": child_action,
        "right": child_action,
        "t2-eval": eval_action,
    }
    tc = TaskCenter(
        spawn_func=_scripted_spawn(scripts),
        request_id="req",
        run_id="run",
        task_center_store=store,
    )
    await tc.run_query("plan it")

    tasks = {t["id"]: t for t in store.list_tasks_for_run("run")}
    assert tasks["run:t1"]["status"] == "done"
    assert tasks["run:t2"]["role"] == "planner"
    assert tasks["run:t2"]["status"] == "done"
    assert tasks["run:left"]["status"] == "done"
    assert tasks["run:right"]["status"] == "done"
    assert tasks["run:right"]["needs"] == ["run:left"]
    assert tasks["run:t2-eval"]["role"] == "evaluator"
    assert tasks["run:t2-eval"]["status"] == "done"

    graphs = store.list_harness_graphs_for_run("run")
    assert len(graphs) == 1
    g = graphs[0]
    assert g["root_task_id"] == "run:t1"
    assert g["planner_task_id"] == "run:t2"
    assert g["evaluator_task_id"] == "run:t2-eval"
    assert sorted(g["executor_task_ids"]) == ["run:left", "run:right"]


@pytest.mark.asyncio
async def test_nested_plan_handoffs_persist_two_harness_graphs() -> None:
    store = _memory_store()
    _create_run(store, request_id="req", run_id="run", prompt="nested")

    async def root_action(tc, tid):
        tc.request_plan(tid, "outer")

    async def outer_planner(tc, tid):
        tc.submit_plan_handoff(tid, [{"id": "x"}], {"x": "complex"}, "outer", "evaluate")

    async def x_action(tc, tid):
        tc.request_plan(tid, "x decompose")

    async def inner_planner(tc, tid):
        tc.submit_plan_handoff(tid, [{"id": "y"}], {"y": "do y"}, "inner", "evaluate")

    async def y_action(tc, tid):
        tc.submit_task_success(tid, "y done")

    async def inner_eval(tc, tid):
        tc.submit_task_success(tid, "inner ok")

    async def outer_eval(tc, tid):
        tc.submit_task_success(tid, "outer ok")

    scripts = {
        "t1": root_action,
        "t2": outer_planner,
        "x": x_action,
        "t3": inner_planner,
        "y": y_action,
        "t3-eval": inner_eval,
        "t2-eval": outer_eval,
    }
    tc = TaskCenter(
        spawn_func=_scripted_spawn(scripts),
        request_id="req",
        run_id="run",
        task_center_store=store,
    )
    await tc.run_query("nested")

    graphs = {g["id"]: g for g in store.list_harness_graphs_for_run("run")}
    # Two harness graphs: outer (parent=t1, planner=t2) and inner (parent=x, planner=t3).
    assert len(graphs) == 2
    parents = {g["root_task_id"] for g in graphs.values()}
    assert parents == {"run:t1", "run:x"}

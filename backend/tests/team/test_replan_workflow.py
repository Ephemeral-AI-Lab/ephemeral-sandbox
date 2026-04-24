from __future__ import annotations

from types import SimpleNamespace

import pytest

from agents.registry import get_definition
from team.definitions import register_all as register_team_builtins
from team.core.models import (
    BudgetConfig,
    BudgetState,
    Task,
    TaskDefinition,
    TaskStatus,
)
from .helpers import make_task as _task
from .helpers import structured_spec as _spec
from team.persistence.task_store import TaskStore
from team.task_center import TaskCenter
from tools.core.base import ToolExecutionContext
from tools.submission import SubmitReplanTool


if get_definition("developer") is None:
    register_team_builtins()


class _FakeSessionFactory:
    def __call__(self):
        class _Ctx:
            async def __aenter__(self_inner):
                return None

            async def __aexit__(self_inner, *args):
                return False

        return _Ctx()


@pytest.mark.asyncio
async def test_submit_replan_inserts_new_tasks_as_replanner_children():
    task_center = SimpleNamespace(
        posted=[],
        notes=None,
        graph={
            "replanner": _task(
                "replanner",
                agent_name="team_replanner",
                status=TaskStatus.RUNNING,
            ),
            "sibling": _task("sibling", status=TaskStatus.EXPANDED),
        },
    )

    async def _post(note):
        task_center.posted.append(note)

    task_center.notes = SimpleNamespace(post=_post)
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "repair",
                    "name": "developer",
                    "spec": _spec("Repair under the replanner."),
                    "scope_paths": ["src/b.py"],
                },
                {
                    "id": "child",
                    "name": "developer",
                    "spec": _spec("Repair under the replanner."),
                    "scope_paths": ["src/a.py"],
                },
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is False, result.output
    replan = ctx.metadata["resolved_plan"]
    assert [task.parent_id for task in replan.add_tasks] == [
        "replanner",
        "replanner",
    ]


@pytest.mark.asyncio
async def test_submit_replan_rejects_cancel_of_non_direct_sibling():
    task_center = SimpleNamespace(
        posted=[],
        notes=SimpleNamespace(post=lambda note: None),
        graph={
            "replanner": _task(
                "replanner",
                agent_name="team_replanner",
                status=TaskStatus.RUNNING,
            ),
            "sibling": _task("sibling", status=TaskStatus.EXPANDED),
            "nested": _task("nested", parent_id="sibling", status=TaskStatus.READY),
        },
    )
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(cancel_ids=["nested"]),
        ctx,
    )

    assert result.is_error is True
    assert "not a direct sibling" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_cancel_outside_siblings():
    task_center = SimpleNamespace(
        posted=[],
        notes=SimpleNamespace(post=lambda note: None),
        graph={
            "replanner": _task(
                "replanner",
                agent_name="team_replanner",
                status=TaskStatus.RUNNING,
            ),
            "outside": _task("outside", parent_id="other-parent", status=TaskStatus.READY),
        },
    )
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(cancel_ids=["outside"]),
        ctx,
    )

    assert result.is_error is True
    assert "not a direct sibling" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_self_original_and_terminal_cancel_ids():
    task_center = SimpleNamespace(
        posted=[],
        notes=SimpleNamespace(post=lambda note: None),
        graph={
            "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
            "replanner": _task(
                "replanner",
                agent_name="team_replanner",
                status=TaskStatus.RUNNING,
                fired_by_task_id="failed",
            ),
            "done": _task("done", status=TaskStatus.DONE),
        },
    )
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(cancel_ids=["replanner", "failed", "done"]),
        ctx,
    )

    assert result.is_error is True
    assert "replanner cannot cancel itself" in result.output
    assert "replanner cannot cancel the original request_replan task" in result.output
    assert "cancel target 'done' is done; cannot cancel" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_dep_on_rewired_downstream_task():
    task_center = SimpleNamespace(
        posted=[],
        notes=SimpleNamespace(post=lambda note: None),
        graph={
            "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
            "replanner": _task(
                "replanner",
                agent_name="team_replanner",
                status=TaskStatus.RUNNING,
                fired_by_task_id="failed",
            ),
            "downstream": _task(
                "downstream",
                status=TaskStatus.PENDING,
                deps=["replanner"],
            ),
        },
    )
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "repair",
                    "name": "developer",
                    "spec": _spec("Invalidly wait for downstream work blocked on R."),
                    "deps": ["downstream"],
                    "scope_paths": ["src/a.py"],
                }
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "unknown dep 'downstream'" in result.output


@pytest.mark.asyncio
async def test_request_replan_is_idempotent_when_live_replanner_exists(monkeypatch):
    """A duplicate request_replan for the same origin must not spawn a
    second replanner — it returns the live one already in flight."""
    from team.persistence import task_store as task_store_module

    store = TaskStore(_FakeSessionFactory(), "run-1")

    source_row = SimpleNamespace(
        id="failed-task",
        parent_id="parent",
        root_id="root",
        depth=1,
        agent_name="developer",
        scope_paths=[],
        status="running",
        fired_by_task_id=None,
    )
    existing_replanner = SimpleNamespace(
        id="existing-replanner",
        team_run_id="run-1",
        agent_name="team_replanner",
        status="ready",
        fired_by_task_id="failed-task",
    )
    inserts: list[object] = []

    async def _fake_fetch(db, team_run_id, task_id):
        return source_row

    async def _fake_find(db, team_run_id, origin_task_id):
        return [existing_replanner]

    async def _fake_insert(db, record):
        inserts.append(record)

    async def _fake_replace(**kwargs):
        raise AssertionError("replace_dependency must not run when reusing replanner")

    async def _fake_set_status(db, team_run_id, task_id, status, reason=None):
        raise AssertionError("set_status must not run when reusing replanner")

    monkeypatch.setattr(task_store_module.q, "fetch_record", _fake_fetch)
    monkeypatch.setattr(
        task_store_module.q, "find_live_tasks_by_fired_origin", _fake_find
    )
    monkeypatch.setattr(task_store_module.q, "insert_task_record", _fake_insert)
    monkeypatch.setattr(task_store_module.q, "replace_dependency", _fake_replace)
    monkeypatch.setattr(task_store_module.q, "set_status", _fake_set_status)

    returned, is_new = await store.request_replan(
        task_id="failed-task",
        reason="boom",
        suggestion=None,
        replanner_agent="team_replanner",
    )

    assert is_new is False
    assert returned is existing_replanner
    assert inserts == []


@pytest.mark.asyncio
async def test_request_replan_inserts_new_replanner_when_none_exists(monkeypatch):
    """Sanity check: when no live replanner exists, request_replan inserts a new one."""
    from team.persistence import task_store as task_store_module

    class _FakeDb:
        async def commit(self) -> None:
            return None

    class _CommittableSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return _FakeDb()

                async def __aexit__(self_inner, *args):
                    return False

            return _Ctx()

    store = TaskStore(_CommittableSessionFactory(), "run-1")

    source_row = SimpleNamespace(
        id="failed-task",
        parent_id="parent",
        root_id="root",
        depth=1,
        agent_name="developer",
        scope_paths=[],
        status="running",
        fired_by_task_id=None,
    )
    inserts: list[object] = []
    replaces: list[dict] = []

    async def _fake_fetch(db, team_run_id, task_id):
        return source_row

    async def _fake_find(db, team_run_id, origin_task_id):
        return []

    async def _fake_insert(db, record):
        inserts.append(record)

    async def _fake_replace(db, team_run_id, *, old_dep_id, new_dep_ids):
        replaces.append({"old": old_dep_id, "new": list(new_dep_ids)})

    async def _fake_set_status(db, team_run_id, task_id, status, reason=None):
        assert status == "request_replan"
        assert reason == "boom"
        pass

    async def _fake_refresh(self):
        return None

    monkeypatch.setattr(task_store_module.q, "fetch_record", _fake_fetch)
    monkeypatch.setattr(
        task_store_module.q, "find_live_tasks_by_fired_origin", _fake_find
    )
    monkeypatch.setattr(task_store_module.q, "insert_task_record", _fake_insert)
    monkeypatch.setattr(task_store_module.q, "replace_dependency", _fake_replace)
    monkeypatch.setattr(task_store_module.q, "set_status", _fake_set_status)
    monkeypatch.setattr(TaskStore, "refresh_graph", _fake_refresh)

    replanner, is_new = await store.request_replan(
        task_id="failed-task",
        reason="boom",
        suggestion=None,
        replanner_agent="team_replanner",
    )

    assert is_new is True
    assert len(inserts) == 1
    assert replanner.fired_by_task_id == "failed-task"
    assert replaces == [
        {"old": "failed-task", "new": [replanner.id]}
    ]


@pytest.mark.asyncio
async def test_apply_replan_atomic_inserts_children_at_replanner_depth(monkeypatch):
    from team.persistence import task_store as task_store_module

    class _FakeDb:
        async def commit(self) -> None:
            return None

    class _CommittableSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return _FakeDb()

                async def __aexit__(self_inner, *args):
                    return False

            return _Ctx()

    store = TaskStore(_CommittableSessionFactory(), "run-1")
    inserted_calls: list[dict[str, object]] = []

    async def _fake_bulk_cancel(db, team_run_id, *, task_ids=None, statuses=None, reason):
        assert task_ids == []
        assert statuses is None
        return 0

    async def _fake_cascade_cancel_recursive(db, team_run_id, task_id):
        return []

    async def _fake_fetch_record(db, team_run_id, parent_id):
        assert parent_id == "replanner"
        return SimpleNamespace(depth=2, root_id="root", id="replanner")

    async def _fake_insert_plan_records(
        db,
        team_run_id,
        specs,
        parent_id,
        parent_depth,
        parent_root_id,
        *,
        child_depth=None,
    ):
        inserted_calls.append(
            {
                "parent_id": parent_id,
                "parent_depth": parent_depth,
                "parent_root_id": parent_root_id,
                "child_depth": child_depth,
                "spec_ids": [spec.id for spec in specs],
            }
        )
        return []

    async def _fake_refresh(self):
        return None

    monkeypatch.setattr(task_store_module.q, "bulk_cancel", _fake_bulk_cancel)
    monkeypatch.setattr(
        task_store_module.q, "cascade_cancel_recursive", _fake_cascade_cancel_recursive
    )
    monkeypatch.setattr(task_store_module.q, "fetch_record", _fake_fetch_record)
    monkeypatch.setattr(task_store_module.q, "insert_plan_records", _fake_insert_plan_records)
    monkeypatch.setattr(TaskStore, "refresh_graph", _fake_refresh)

    await store.apply_replan_atomic(
        cancel_ids=[],
        cancel_reason="cancelled_by_replan_replanner",
        specs=[
            TaskDefinition(
                id="same-depth-repair",
                spec=_spec("Repair at replanner depth."),
                agent="developer",
                description="repair",
                scope_paths=["src/parser.py"],
                parent_id="replanner",
            )
        ],
    )

    assert inserted_calls == [
        {
            "parent_id": "replanner",
            "parent_depth": 2,
            "parent_root_id": "root",
            "child_depth": 2,
            "spec_ids": ["same-depth-repair"],
        }
    ]


@pytest.mark.asyncio
async def test_replanner_context_includes_root_cause_trace_and_rewired_dependents():
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    tc.graph.update(
        {
            "parent": _task("parent", agent_name="team_planner", status=TaskStatus.EXPANDED),
            "dep": _task("dep", status=TaskStatus.DONE),
            "failed": Task(
                id="failed",
                team_run_id="run-1",
                agent_name="developer",
                status=TaskStatus.REQUEST_REPLAN,
                spec=_spec("Fix the parser."),
                description="Original detailed task description.",
                deps=["dep"],
                scope_paths=["src/parser.py"],
                parent_id="parent",
                root_id="root",
                depth=1,
                failure_reason="replan_requested: parser failure",
            ),
            "replanner": Task(
                id="replanner",
                team_run_id="run-1",
                agent_name="team_replanner",
                status=TaskStatus.READY,
                spec=_spec("Replan failed parser task."),
                scope_paths=["src/parser.py"],
                parent_id="parent",
                root_id="root",
                depth=1,
                fired_by_task_id="failed",
            ),
            "downstream": _task(
                "downstream",
                status=TaskStatus.PENDING,
                deps=["replanner"],
            ),
        }
    )
    context = await tc.context.context_for(tc.graph["replanner"])

    assert "## Replan root cause trace" in context
    assert "Original task: failed" in context
    assert "Fix the parser." in context
    assert "Original detailed task description." in context
    assert "replan_requested: parser failure" in context
    assert "downstream (pending); deps: replanner" in context

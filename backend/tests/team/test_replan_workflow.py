from __future__ import annotations

from types import SimpleNamespace

import pytest

from agents.registry import get_definition
from team.builtins import register_all as register_team_builtins
from team.models import (
    BudgetConfig,
    BudgetState,
    Note,
    Task,
    TaskDefinition,
    TaskStatus,
)
from .helpers import make_task as _task
from .helpers import structured_spec as _spec
from team.persistence.task_store import TaskStore
from team.task_center import TaskCenter
from tools.core.base import ToolExecutionContext
from tools.submission.toolkit import SubmitReplanTool


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
                    "description": "Repair under replanner",
                    "spec": _spec("Repair under the replanner."),
                    "scope_paths": ["src/b.py"],
                },
                {
                    "id": "child",
                    "name": "developer",
                    "description": "Child repair",
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
                    "description": "Invalid downstream dep",
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


class _FakeExpander:
    def __init__(self, outcome: SimpleNamespace) -> None:
        self.outcome = outcome

    async def expand_submitted_plan(self, rec, result):
        del rec, result
        return SimpleNamespace(accepted=True, new_items=())

    async def apply_replan(self, **kwargs):
        del kwargs
        return self.outcome


def _replan_outcome(
    *,
    replanner_child_count: int = 0,
) -> SimpleNamespace:
    return SimpleNamespace(
        replanner_child_count=replanner_child_count,
    )


class _FakeStore:
    def __init__(self, graph: dict[str, Task]) -> None:
        self.graph = graph
        self.marked_done: list[str] = []
        self.marked_expanded: list[str] = []
        self.expanded_promotions: dict[str, list[str]] = {}

    async def get_record(self, task_id: str):
        task = self.graph.get(task_id)
        if task is None:
            return None
        return SimpleNamespace(
            id=task.id,
            status=task.status.value,
            depth=task.depth,
            parent_id=task.parent_id,
            root_id=task.root_id,
            fired_by_task_id=task.fired_by_task_id,
        )

    async def mark_done(self, task_id: str):
        self.marked_done.append(task_id)
        self.graph[task_id].status = TaskStatus.DONE
        promoted = []
        for task in self.graph.values():
            if task.status == TaskStatus.PENDING and task_id in task.deps:
                if all(
                    self.graph.get(dep_id) is not None
                    and self.graph[dep_id].status == TaskStatus.DONE
                    for dep_id in task.deps
                ):
                    task.status = TaskStatus.READY
                    promoted.append(task.id)
        return promoted

    async def mark_expanded(self, task_id: str) -> None:
        self.marked_expanded.append(task_id)
        self.graph[task_id].status = TaskStatus.EXPANDED

    async def maybe_promote_expanded_parent(self, child_id: str):
        promoted = self.expanded_promotions.get(child_id, [])
        for task_id in promoted:
            self.graph[task_id].status = TaskStatus.DONE
        return promoted, []

    async def sweep_expanded_promotions(self):
        return [], []

    async def finalize_replanned_origin(self, replanner_task_id: str):
        replanner = self.graph[replanner_task_id]
        origin_id = replanner.fired_by_task_id
        if not origin_id:
            return None
        # REQUEST_REPLAN is terminal: record the recovery linkage without
        # transitioning A's status.
        self.graph[origin_id].failure_reason = f"replanned_by:{replanner_task_id}"
        return origin_id

    async def refresh_graph(self):
        return self.graph


def _task_center_with_store(store: _FakeStore, expander: _FakeExpander) -> TaskCenter:
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    tc._store = store
    tc._expander = expander
    return tc


class _RecordingEventStore:
    def __init__(self) -> None:
        self.events = []

    def append(self, event) -> None:
        self.events.append(event)

    def load_run(self, team_run_id: str):
        return list(self.events)

    def list_runs(self):
        return ["run-1"] if self.events else []


class _ReusingReplanStore(_FakeStore):
    def __init__(self, graph: dict[str, Task]) -> None:
        super().__init__(graph)
        self.request_replan_calls = 0

    async def request_replan(
        self,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ):
        del reason, suggestion
        self.request_replan_calls += 1
        if self.request_replan_calls == 1:
            origin = self.graph[task_id]
            origin.status = TaskStatus.REQUEST_REPLAN
            self.graph["replanner"] = _task(
                "replanner",
                agent_name=replanner_agent,
                status=TaskStatus.READY,
                parent_id=origin.parent_id,
                fired_by_task_id=task_id,
            )
            return self.graph["replanner"], True
        return self.graph["replanner"], False




class _RewiringReplanStore(_FakeStore):
    async def request_replan(
        self,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ):
        del reason, suggestion
        origin = self.graph[task_id]
        origin.status = TaskStatus.REQUEST_REPLAN
        replanner = _task(
            "replanner",
            agent_name=replanner_agent,
            status=TaskStatus.READY,
            parent_id=origin.parent_id,
            fired_by_task_id=task_id,
        )
        self.graph["replanner"] = replanner
        for task in self.graph.values():
            if task.status == TaskStatus.PENDING and task_id in task.deps:
                task.deps = ["replanner" if dep == task_id else dep for dep in task.deps]
        return replanner, True


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

    async def _fake_set_status(db, team_run_id, task_id, reason):
        raise AssertionError("set_status_request_replan must not run when reusing replanner")

    monkeypatch.setattr(task_store_module.q, "fetch_replan_source", _fake_fetch)
    monkeypatch.setattr(
        task_store_module.q, "find_live_tasks_by_fired_origin", _fake_find
    )
    monkeypatch.setattr(task_store_module.q, "insert_task_record", _fake_insert)
    monkeypatch.setattr(task_store_module.q, "replace_dependency", _fake_replace)
    monkeypatch.setattr(
        task_store_module.q, "set_status_request_replan", _fake_set_status
    )

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

    async def _fake_set_status(db, team_run_id, task_id, reason):
        pass

    async def _fake_refresh(self):
        return None

    monkeypatch.setattr(task_store_module.q, "fetch_replan_source", _fake_fetch)
    monkeypatch.setattr(
        task_store_module.q, "find_live_tasks_by_fired_origin", _fake_find
    )
    monkeypatch.setattr(task_store_module.q, "insert_task_record", _fake_insert)
    monkeypatch.setattr(task_store_module.q, "replace_dependency", _fake_replace)
    monkeypatch.setattr(
        task_store_module.q, "set_status_request_replan", _fake_set_status
    )
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

    async def _fake_cancel_by_ids(db, team_run_id, task_ids, reason):
        return 0

    async def _fake_cascade_cancel_recursive(db, team_run_id, task_id):
        return []

    async def _fake_fetch_parent_depth_and_root(db, team_run_id, parent_id):
        assert parent_id == "replanner"
        return 2, "root"

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

    monkeypatch.setattr(task_store_module.q, "cancel_by_ids", _fake_cancel_by_ids)
    monkeypatch.setattr(
        task_store_module.q, "cascade_cancel_recursive", _fake_cascade_cancel_recursive
    )
    monkeypatch.setattr(
        task_store_module.q,
        "fetch_parent_depth_and_root",
        _fake_fetch_parent_depth_and_root,
    )
    monkeypatch.setattr(task_store_module.q, "insert_plan_records", _fake_insert_plan_records)
    monkeypatch.setattr(TaskStore, "refresh_graph", _fake_refresh)

    await store.apply_replan_atomic(
        cancel_ids=[],
        cancel_reason="cancelled_by_replan_replanner",
        specs=[
            TaskDefinition(
                id="same-depth-repair",
                objective=_spec("Repair at replanner depth."),
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
                objective="1. Goal: Fix the parser.",
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
                objective="Replan failed parser task.",
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
    await tc.notes.post(
        Note(
            id="n-dep",
            task_id="dep",
            agent_name="developer",
            content="Dependency produced parser setup.",
        )
    )
    await tc.notes.post(
        Note(
            id="n-failed",
            task_id="failed",
            agent_name="developer",
            content="Parser failed because the grammar changed.",
        )
    )
    await tc.notes.post(
        Note(
            id="n-parent",
            task_id="parent",
            agent_name="team_planner",
            content="Parent split parser work from lexer work.",
        )
    )

    context = await tc.context.context_for(tc.graph["replanner"])

    assert "## Replan root cause trace" in context
    assert "Original task: failed" in context
    assert "1. Goal: Fix the parser." in context
    assert "Original detailed task description." in context
    assert "replan_requested: parser failure" in context
    assert "Dependency produced parser setup." in context
    assert "Parser failed because the grammar changed." in context
    assert "downstream (pending); deps: replanner" in context
    assert "Parent split parser work from lexer work." in context


@pytest.mark.asyncio
async def test_parent_summarizer_context_skips_replanner_failure_trace():
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    tc.graph.update(
        {
            "parent": _task("parent", agent_name="team_planner", status=TaskStatus.EXPANDED),
            "summary": Task(
                id="summary",
                team_run_id="run-1",
                agent_name="parent_summarizer",
                status=TaskStatus.READY,
                objective="Summarize parent task after children finish.",
                parent_id="parent",
                root_id="root",
                depth=1,
                fired_by_task_id="parent",
            ),
        }
    )
    await tc.notes.post(
        Note(
            id="n-parent",
            task_id="parent",
            agent_name="team_planner",
            content="Parent split parser work from lexer work.",
        )
    )

    context = await tc.context.context_for(tc.graph["summary"])
    parts = await tc.context.template_context_for(tc.graph["summary"])

    assert "Summarize parent task after children finish." in context
    assert "Parent split parser work from lexer work." in context
    assert "## Replan root cause trace" not in context
    assert "Failed reason:" not in context
    assert "Downstream dependents rewired to this replanner" not in context
    assert parts.failure_context == ""

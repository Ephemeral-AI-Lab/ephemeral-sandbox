from __future__ import annotations

from types import SimpleNamespace

import pytest
from pydantic import ValidationError

from agents.registry import get_definition
from team.builtins import register_all as register_team_builtins
from team.errors import GraphInvariantViolation, InvalidPlan
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Note,
    ReplanPlan,
    ReplanRequest,
    Task,
    TaskDefinition,
    TaskStatus,
)
from team.planning.expander import PlanExpander, PlanExpansionOutcome, ReplanApplyOutcome
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


def _task(
    task_id: str,
    *,
    agent_name: str = "developer",
    status: TaskStatus = TaskStatus.PENDING,
    parent_id: str | None = "parent",
    deps: list[str] | None = None,
    fired_by_task_id: str | None = None,
) -> Task:
    return Task(
        id=task_id,
        team_run_id="run-1",
        agent_name=agent_name,
        status=status,
        objective=f"task {task_id}",
        deps=deps or [],
        parent_id=parent_id,
        root_id="root",
        depth=1 if parent_id else 0,
        fired_by_task_id=fired_by_task_id,
    )


def _spec(text: str = "Do the work.") -> str:
    return (
        f"1. Goal: {text}\n"
        "2. Environment: Use the current repository workspace.\n"
        "3. Scope: Stay within scope_paths.\n"
        "4. Context: Created by a replanner.\n"
        "5. Acceptance Criteria: Submit the terminal outcome."
    )


@pytest.mark.asyncio
async def test_replace_run_tasks_rejects_ready_snapshot_with_failed_dependency():
    store = TaskStore(_FakeSessionFactory(), "run-1")

    with pytest.raises(GraphInvariantViolation, match="failed"):
        await store.replace_run_tasks(
            [
                _task("failed", status=TaskStatus.FAILED),
                _task(
                    "reviewer-dependent",
                    agent_name="reviewer",
                    status=TaskStatus.READY,
                    deps=["failed"],
                ),
            ]
        )


@pytest.mark.asyncio
async def test_replace_run_tasks_rejects_running_snapshot_with_pending_dependency():
    store = TaskStore(_FakeSessionFactory(), "run-1")

    with pytest.raises(GraphInvariantViolation, match="pending"):
        await store.replace_run_tasks(
            [
                _task("pending", status=TaskStatus.PENDING),
                _task("running-dependent", status=TaskStatus.RUNNING, deps=["pending"]),
            ]
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "status",
    [TaskStatus.EXPANDED, TaskStatus.REQUEST_REPLAN, TaskStatus.DONE],
)
async def test_replace_run_tasks_rejects_post_ready_snapshot_statuses_with_pending_dependency(
    status: TaskStatus,
):
    store = TaskStore(_FakeSessionFactory(), "run-1")

    with pytest.raises(GraphInvariantViolation, match="pending"):
        await store.replace_run_tasks(
            [
                _task("pending", status=TaskStatus.PENDING),
                _task("dependent", status=status, deps=["pending"]),
            ]
        )


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


def test_submit_replan_rejects_unknown_fields():
    # Sibling buckets are no longer accepted; pydantic should
    # reject it via ConfigDict(extra="forbid").
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(new_sibling_tasks=[])
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(new_children_tasks=[])
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "legacy-parent",
                    "name": "developer",
                    "description": "Legacy parent placement",
                    "spec": _spec("Legacy parent placement should be rejected."),
                    "parent_id": "parent",
                }
            ]
        )


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
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "unknown dep 'downstream'" in result.output


class _FakeExpander:
    def __init__(self, outcome: ReplanApplyOutcome) -> None:
        self.outcome = outcome

    async def expand_submitted_plan(self, rec, result):
        return PlanExpansionOutcome.accepted_with()

    async def apply_replan(self, **kwargs):
        return self.outcome


def _replan_outcome(
    *,
    added: int = 0,
    cancelled: int = 0,
    inserted_ids: tuple[str, ...] = (),
    replanner_child_count: int = 0,
) -> ReplanApplyOutcome:
    return ReplanApplyOutcome(
        added=added,
        cancelled=cancelled,
        inserted_ids=inserted_ids,
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
        return promoted

    async def sweep_expanded_promotions(self):
        return []

    async def finalize_replanned_origin(self, replanner_task_id: str):
        replanner = self.graph[replanner_task_id]
        origin_id = replanner.fired_by_task_id
        if not origin_id:
            return None
        self.graph[origin_id].status = TaskStatus.FAILED
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
    tc._transitions._graph_getter = lambda: store.graph
    tc._transitions._refresh_graph_fn = store.refresh_graph
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


@pytest.mark.asyncio
async def test_request_replan_double_call_emits_task_added_once():
    graph = {"failed": _task("failed", status=TaskStatus.RUNNING)}
    store = _ReusingReplanStore(graph)
    event_store = _RecordingEventStore()
    tc = _task_center_with_store(
        store,
        _FakeExpander(_replan_outcome()),
    )
    tc._events = event_store

    request = ReplanRequest(reason="boom", suggestion=None)
    first = await tc.request_replan("failed", request)
    second = await tc.request_replan("failed", request)

    assert first.id == "replanner"
    assert second.id == "replanner"
    task_added_events = [event for event in event_store.events if event.kind == "task_added"]
    assert len(task_added_events) == 1
    assert task_added_events[0].data["task"]["id"] == "replanner"
    assert [event.kind for event in event_store.events].count("budget_update") == 1


@pytest.mark.asyncio
async def test_replanner_done_immediately_when_replan_has_no_children():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
        "downstream": _task("downstream", deps=["replanner"]),
    }
    store = _FakeStore(graph)
    tc = _task_center_with_store(
        store,
        _FakeExpander(_replan_outcome()),
    )

    await tc.complete_task("replanner", AgentResult(summary="", submitted_replan=ReplanPlan()))

    assert graph["replanner"].status == TaskStatus.DONE
    assert graph["failed"].status == TaskStatus.FAILED
    assert graph["downstream"].status == TaskStatus.READY
    assert store.marked_expanded == []


@pytest.mark.asyncio
async def test_replanner_expanded_when_replan_creates_direct_children():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    store = _FakeStore(graph)
    tc = _task_center_with_store(
        store,
        _FakeExpander(
            _replan_outcome(added=1, inserted_ids=("child",), replanner_child_count=1)
        ),
    )

    await tc.complete_task("replanner", AgentResult(summary="", submitted_replan=ReplanPlan()))

    assert graph["replanner"].status == TaskStatus.EXPANDED
    assert graph["failed"].status == TaskStatus.REQUEST_REPLAN
    assert store.marked_done == []


@pytest.mark.asyncio
async def test_expanded_replanner_finalizes_origin_after_successful_child_completion():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.EXPANDED,
            fired_by_task_id="failed",
        ),
        "child": _task("child", status=TaskStatus.RUNNING, parent_id="replanner"),
    }
    store = _FakeStore(graph)
    store.expanded_promotions["child"] = ["replanner"]
    tc = _task_center_with_store(store, _FakeExpander(_replan_outcome()))

    await tc.complete_task("child", AgentResult(summary="done"))

    assert graph["child"].status == TaskStatus.DONE
    assert graph["replanner"].status == TaskStatus.DONE
    assert graph["failed"].status == TaskStatus.FAILED


@pytest.mark.asyncio
async def test_finalized_replan_origin_can_promote_ancestor_parent():
    graph = {
        "parent": _task("parent", status=TaskStatus.EXPANDED, parent_id=None),
        "failed": _task(
            "failed",
            status=TaskStatus.REQUEST_REPLAN,
            parent_id="parent",
        ),
        "sibling": _task("sibling", status=TaskStatus.DONE, parent_id="parent"),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.EXPANDED,
            parent_id="parent",
            fired_by_task_id="failed",
        ),
        "child": _task("child", status=TaskStatus.RUNNING, parent_id="replanner"),
    }
    store = _FakeStore(graph)
    store.expanded_promotions["child"] = ["replanner"]
    store.expanded_promotions["failed"] = ["parent"]
    tc = _task_center_with_store(store, _FakeExpander(_replan_outcome()))

    await tc.complete_task("child", AgentResult(summary="done"))

    assert graph["child"].status == TaskStatus.DONE
    assert graph["replanner"].status == TaskStatus.DONE
    assert graph["failed"].status == TaskStatus.FAILED
    assert graph["parent"].status == TaskStatus.DONE


class _ExpanderStore:
    def __init__(self, graph: dict[str, Task]) -> None:
        self.graph = graph
        self.calls: list[str] = []

    async def get_adjacency(self):
        return {task_id: list(task.deps) for task_id, task in self.graph.items()}

    async def insert_plan(self, specs, **kwargs):
        self.calls.append("insert_plan")
        return []

    async def apply_replan_atomic(self, **kwargs):
        self.calls.append("apply_replan_atomic")
        return len(kwargs["cancel_ids"]), []


class _Budget:
    def __init__(self) -> None:
        self.charged = 0
        self.added = 0
        self.budgets = BudgetConfig()

    def has_capacity_for(self, count: int) -> bool:
        return True

    def charge_tasks(self, count: int) -> None:
        self.charged += count

    def add_tasks_used(self, count: int) -> None:
        self.added += count

    def within_depth_limit(self, new_depth: int) -> bool:
        return new_depth <= self.budgets.max_depth

    def emit_update(self) -> None:
        return None


@pytest.mark.asyncio
async def test_replan_cancels_active_runner_after_apply_replan_atomic_commits():
    graph = {
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
        ),
        "running-target": _task("running-target", status=TaskStatus.RUNNING),
    }
    store = _ExpanderStore(graph)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
        cancel_running_task_cb=lambda task_id: store.calls.append(f"cancel:{task_id}"),
    )

    await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=["running-target"],
    )

    # DB transaction commits BEFORE runtime cancellation so a rollback
    # cannot leave the graph saying the task is RUNNING while its runner
    # has already been killed.
    assert store.calls == ["apply_replan_atomic", "cancel:running-target"]


@pytest.mark.asyncio
async def test_replan_cancel_cascade_includes_reviewer_dependents():
    graph = {
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
        ),
        "running-target": _task("running-target", status=TaskStatus.RUNNING),
        "reviewer-dependent": _task(
            "reviewer-dependent",
            agent_name="reviewer",
            status=TaskStatus.RUNNING,
            deps=["running-target"],
        ),
    }
    store = _ExpanderStore(graph)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
        cancel_running_task_cb=lambda task_id: store.calls.append(f"cancel:{task_id}"),
    )

    await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=["running-target"],
    )

    assert store.calls == [
        "apply_replan_atomic",
        "cancel:reviewer-dependent",
        "cancel:running-target",
    ]


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
        return existing_replanner

    async def _fake_insert(db, record):
        inserts.append(record)

    async def _fake_replace(**kwargs):
        raise AssertionError("replace_dependency must not run when reusing replanner")

    async def _fake_set_status(db, team_run_id, task_id, reason):
        raise AssertionError("set_status_request_replan must not run when reusing replanner")

    monkeypatch.setattr(task_store_module.q, "fetch_replan_source", _fake_fetch)
    monkeypatch.setattr(
        task_store_module.q, "find_live_replanner_for_origin", _fake_find
    )
    monkeypatch.setattr(task_store_module.q, "insert_replanner_record", _fake_insert)
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
        return None

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
        task_store_module.q, "find_live_replanner_for_origin", _fake_find
    )
    monkeypatch.setattr(task_store_module.q, "insert_replanner_record", _fake_insert)
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
async def test_replan_does_not_cancel_runner_when_apply_replan_atomic_raises():
    """If apply_replan_atomic fails, no live runner cancellation happens,
    so graph state and runner state stay consistent under rollback."""

    class _RaisingStore(_ExpanderStore):
        async def apply_replan_atomic(self, **kwargs):
            self.calls.append("apply_replan_atomic")
            raise RuntimeError("db commit failed")

    graph = {
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
        ),
        "running-target": _task("running-target", status=TaskStatus.RUNNING),
    }
    store = _RaisingStore(graph)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
        cancel_running_task_cb=lambda task_id: store.calls.append(f"cancel:{task_id}"),
    )

    with pytest.raises(RuntimeError, match="db commit failed"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[],
            cancel_ids=["running-target"],
        )

    assert store.calls == ["apply_replan_atomic"]


@pytest.mark.asyncio
async def test_replan_expander_rejects_original_task_cancellation():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
    )

    with pytest.raises(InvalidPlan, match="original request_replan task"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[],
            cancel_ids=["failed"],
        )


@pytest.mark.asyncio
async def test_replan_expander_rejects_insertion_under_original_task():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
    )

    with pytest.raises(InvalidPlan, match="must be direct children of the replanner"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[
                TaskDefinition(
                    id="bad-child",
                    objective=_spec("Invalid repair under original failed task."),
                    agent="developer",
                    parent_id="failed",
                )
            ],
            cancel_ids=[],
        )


@pytest.mark.asyncio
async def test_replan_expander_applies_plan_policy_to_added_tasks():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
    )

    with pytest.raises(InvalidPlan, match="submitted plans cannot include replanner agent"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[
                TaskDefinition(
                    id="bad-replanner",
                    objective=_spec("Invalid replanner target."),
                    agent="team_replanner",
                    description="invalid replanner target",
                    scope_paths=["src/a.py"],
                    parent_id="replanner",
                )
            ],
            cancel_ids=[],
        )


@pytest.mark.asyncio
async def test_replan_expander_rejects_dep_on_rewired_downstream_task():
    graph = {
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
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
        fail_cb=lambda task_id, reason: None,
    )

    with pytest.raises(InvalidPlan, match="unknown dep reference 'downstream'"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[
                TaskDefinition(
                    id="repair",
                    objective=_spec("Invalidly wait for downstream work blocked on R."),
                    agent="developer",
                    description="invalid downstream dependency",
                    deps=["downstream"],
                    scope_paths=["src/a.py"],
                    parent_id="replanner",
                )
            ],
            cancel_ids=[],
        )


@pytest.mark.asyncio
async def test_replanner_context_includes_failure_packet_and_rewired_dependents():
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

    assert "## Replan failure packet" in context
    assert "Original task: failed" in context
    assert "1. Goal: Fix the parser." in context
    assert "Original detailed task description." in context
    assert "replan_requested: parser failure" in context
    assert "Dependency produced parser setup." in context
    assert "Parser failed because the grammar changed." in context
    assert "downstream (pending); deps: replanner" in context
    assert "Parent split parser work from lexer work." in context

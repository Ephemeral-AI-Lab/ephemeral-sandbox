from __future__ import annotations

from types import SimpleNamespace

import pytest

from agents.registry import get_definition
from team.builtins import register_all as register_team_builtins
from team.errors import GraphInvariantViolation, InvalidPlan
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Note,
    ReplanPlan,
    Task,
    TaskDefinition,
    TaskStatus,
)
from team.planning.expander import PlanExpander
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


def test_request_replan_dependency_rewrite_requires_pending_dependents():
    for status in ("ready", "running", "expanded", "replanning", "done", "failed", "cancelled"):
        with pytest.raises(GraphInvariantViolation, match="must be pending"):
            TaskStore._pending_dependency_rewrite_updates(
                [SimpleNamespace(id=f"{status}-dependent", status=status, deps=["failed"])],
                old_dep_id="failed",
                new_dep_ids=["replanner"],
            )


def test_request_replan_dependency_rewrite_updates_pending_dependents():
    updates = TaskStore._pending_dependency_rewrite_updates(
        [
            SimpleNamespace(id="pending-dependent", status="pending", deps=["dep-1", "failed"]),
            SimpleNamespace(id="duplicate-dependent", status="pending", deps=["failed", "dep-2"]),
        ],
        old_dep_id="failed",
        new_dep_ids=["replanner"],
    )

    assert updates == {
        "pending-dependent": ["dep-1", "replanner"],
        "duplicate-dependent": ["dep-2", "replanner"],
    }


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
    [TaskStatus.EXPANDED, TaskStatus.REPLANNING, TaskStatus.DONE],
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
async def test_submit_replan_allows_child_sibling_and_sibling_subtree_insertions():
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
            "sibling-child": _task("sibling-child", parent_id="sibling"),
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
                    "id": "child",
                    "parent_id": "replanner",
                    "name": "developer",
                    "spec": _spec("Repair under the replanner."),
                    "scope_paths": ["src/a.py"],
                },
                {
                    "id": "sibling-new",
                    "parent_id": "parent",
                    "name": "developer",
                    "spec": _spec("Repair beside the replanner."),
                    "scope_paths": ["src/b.py"],
                },
                {
                    "id": "subtree-new",
                    "parent_id": "sibling",
                    "name": "developer",
                    "spec": _spec("Repair inside a surviving sibling subtree."),
                    "scope_paths": ["src/c.py"],
                },
            ],
        ),
        ctx,
    )

    assert result.is_error is False, result.output
    replan = ctx.metadata["resolved_plan"]
    assert [task.parent_id for task in replan.add_tasks] == [
        "replanner",
        "parent",
        "sibling",
    ]


@pytest.mark.asyncio
async def test_submit_replan_allows_non_sibling_cancel_inside_projection():
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
            "nested": _task("nested", parent_id="sibling", status=TaskStatus.READY),
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
        SubmitReplanTool.input_model(cancel_ids=["nested"]),
        ctx,
    )

    assert result.is_error is False, result.output
    assert ctx.metadata["resolved_plan"].cancel_ids == ["nested"]


@pytest.mark.asyncio
async def test_submit_replan_rejects_cancel_outside_projection():
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
    assert "outside the parent projection" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_self_original_and_terminal_cancel_ids():
    task_center = SimpleNamespace(
        posted=[],
        notes=SimpleNamespace(post=lambda note: None),
        graph={
            "failed": _task("failed", status=TaskStatus.REPLANNING),
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
    assert "replanner cannot cancel the original replanning task" in result.output
    assert "cancel target 'done' is done; cannot cancel" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_new_task_under_original_replanning_task():
    task_center = SimpleNamespace(
        posted=[],
        notes=SimpleNamespace(post=lambda note: None),
        graph={
            "failed": _task("failed", status=TaskStatus.REPLANNING),
            "replanner": _task(
                "replanner",
                agent_name="team_replanner",
                status=TaskStatus.RUNNING,
                fired_by_task_id="failed",
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
                    "id": "bad-child",
                    "parent_id": "failed",
                    "name": "developer",
                    "spec": _spec("Invalid repair under the original failed task."),
                    "scope_paths": ["src/a.py"],
                },
            ],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "parent_id 'failed' is outside" in result.output


class _FakeExpander:
    def __init__(self, outcome: dict[str, int | list[str]]) -> None:
        self.outcome = outcome

    async def expand_submitted_plan(self, rec, result):
        return [], True

    async def apply_replan(self, **kwargs):
        return dict(self.outcome)


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
                task.pending_dep_count = max(0, task.pending_dep_count - 1)
                if task.pending_dep_count == 0:
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


@pytest.mark.asyncio
async def test_replanner_done_immediately_when_replan_has_no_children():
    graph = {
        "failed": _task("failed", status=TaskStatus.REPLANNING),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
        "downstream": _task("downstream", deps=["replanner"]),
    }
    graph["downstream"].pending_dep_count = 1
    store = _FakeStore(graph)
    tc = _task_center_with_store(
        store,
        _FakeExpander({"added": 0, "cancelled": 0, "inserted_ids": [], "replanner_child_count": 0}),
    )

    await tc.complete_task("replanner", AgentResult(summary="", submitted_replan=ReplanPlan()))

    assert graph["replanner"].status == TaskStatus.DONE
    assert graph["failed"].status == TaskStatus.FAILED
    assert graph["downstream"].status == TaskStatus.READY
    assert store.marked_expanded == []


@pytest.mark.asyncio
async def test_replanner_expanded_when_replan_creates_direct_children():
    graph = {
        "failed": _task("failed", status=TaskStatus.REPLANNING),
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
            {"added": 1, "cancelled": 0, "inserted_ids": ["child"], "replanner_child_count": 1}
        ),
    )

    await tc.complete_task("replanner", AgentResult(summary="", submitted_replan=ReplanPlan()))

    assert graph["replanner"].status == TaskStatus.EXPANDED
    assert graph["failed"].status == TaskStatus.REPLANNING
    assert store.marked_done == []


@pytest.mark.asyncio
async def test_expanded_replanner_finalizes_origin_after_successful_child_completion():
    graph = {
        "failed": _task("failed", status=TaskStatus.REPLANNING),
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
    tc = _task_center_with_store(store, _FakeExpander({"added": 0, "cancelled": 0}))

    await tc.complete_task("child", AgentResult(summary="done"))

    assert graph["child"].status == TaskStatus.DONE
    assert graph["replanner"].status == TaskStatus.DONE
    assert graph["failed"].status == TaskStatus.FAILED


class _ExpanderStore:
    def __init__(self, graph: dict[str, Task]) -> None:
        self.graph = graph
        self.calls: list[str] = []

    async def get_record(self, task_id: str):
        task = self.graph.get(task_id)
        if task is None:
            return None
        return SimpleNamespace(id=task.id, status=task.status.value, parent_id=task.parent_id)

    async def get_adjacency(self):
        return {task_id: list(task.deps) for task_id, task in self.graph.items()}

    async def apply_replan_atomic(self, **kwargs):
        self.calls.append("apply_replan_atomic")
        return len(kwargs["cancel_ids"]), []


class _Budget:
    def __init__(self) -> None:
        self.charged = 0

    def has_capacity_for(self, count: int) -> bool:
        return True

    def charge_tasks(self, count: int) -> None:
        self.charged += count


@pytest.mark.asyncio
async def test_replan_cancels_active_runner_before_marking_running_task_cancelled():
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
        cascade_fail_cb=lambda task_id, reason: None,
        cancel_active_task_cb=lambda task_id: store.calls.append(f"cancel:{task_id}") is None,
    )

    await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=["running-target"],
        target_parent_id="parent",
    )

    assert store.calls == ["cancel:running-target", "apply_replan_atomic"]


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
        cascade_fail_cb=lambda task_id, reason: None,
        cancel_active_task_cb=lambda task_id: store.calls.append(f"cancel:{task_id}") is None,
    )

    await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=["running-target"],
        target_parent_id="parent",
    )

    assert store.calls == [
        "cancel:reviewer-dependent",
        "cancel:running-target",
        "apply_replan_atomic",
    ]


@pytest.mark.asyncio
async def test_replan_expander_rejects_original_task_cancellation():
    graph = {
        "failed": _task("failed", status=TaskStatus.REPLANNING),
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
        cascade_fail_cb=lambda task_id, reason: None,
    )

    with pytest.raises(InvalidPlan, match="original replanning task"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[],
            cancel_ids=["failed"],
            target_parent_id="parent",
        )


@pytest.mark.asyncio
async def test_replan_expander_rejects_insertion_under_original_task():
    graph = {
        "failed": _task("failed", status=TaskStatus.REPLANNING),
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
        cascade_fail_cb=lambda task_id, reason: None,
    )

    with pytest.raises(InvalidPlan, match="outside the allowed parent projection"):
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
            target_parent_id="parent",
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
                status=TaskStatus.REPLANNING,
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

    context = await tc.notes.context_for(tc.graph["replanner"])

    assert "## Replan failure packet" in context
    assert "Original task: failed" in context
    assert "1. Goal: Fix the parser." in context
    assert "Original detailed task description." in context
    assert "replan_requested: parser failure" in context
    assert "Dependency produced parser setup." in context
    assert "Parser failed because the grammar changed." in context
    assert "downstream (pending); deps: replanner" in context
    assert "Parent split parser work from lexer work." in context

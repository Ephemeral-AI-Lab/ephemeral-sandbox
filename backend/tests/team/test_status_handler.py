"""Unit tests for TaskStatusHandler core match-block cases."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from team.models import (
    BudgetConfig,
    BudgetState,
    Plan,
    Task,
    TaskDefinition,
    TaskStatus,
    TaskStatusUpdate,
)
from team.runtime.status_handler import TaskStatusHandler


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------


def _task(
    task_id: str,
    *,
    status: TaskStatus = TaskStatus.READY,
    agent_name: str = "developer",
    fired_by_task_id: str | None = None,
) -> Task:
    return Task(
        id=task_id,
        team_run_id="run-1",
        agent_name=agent_name,
        status=status,
        objective="do something",
        fired_by_task_id=fired_by_task_id,
    )


class FakeStore:
    """In-memory fake that satisfies TaskStatusHandler's store interface."""

    def __init__(self) -> None:
        self.graph: dict[str, Task] = {}
        self.ready_queue_order: list[str] = []

        # Async methods with configurable return values
        self.mark_done = AsyncMock(return_value=[])
        self.mark_expanded = AsyncMock()
        self.mark_failed = AsyncMock()
        self.mark_terminal = AsyncMock()
        self.refresh_graph = AsyncMock()
        self.get_record = AsyncMock(return_value=None)
        self.maybe_promote_expanded_parent = AsyncMock(return_value=([], []))
        self.finalize_parent_summary = AsyncMock(return_value=[])
        self.finalize_replanned_origin = AsyncMock(return_value=None)
        self.sweep_expanded_promotions = AsyncMock(return_value=([], []))
        self.request_replan = AsyncMock()
        self.insert_parent_summary_task = AsyncMock()
        self.cascade_cancel_recursive = AsyncMock()

    def get_task(self, task_id: str) -> Task | None:
        return self.graph.get(task_id)


class FakeBudget:
    """Minimal BudgetManager-like fake."""

    def __init__(self) -> None:
        self.budgets = BudgetConfig()
        self.budget_state = BudgetState()

    def require_replan_capacity(self) -> None:
        pass

    def bump_replan_counters(self) -> None:
        pass

    def emit_update(self) -> None:
        pass


class FakeExpander:
    """Fake PlanExpander that records calls."""

    def __init__(self, new_tasks: list[Task] | None = None) -> None:
        self._new_tasks = new_tasks or []
        self.expand_submitted_plan = AsyncMock(
            return_value=SimpleNamespace(new_items=self._new_tasks)
        )
        self.apply_replan = AsyncMock()


class FakeNoteManager:
    """Fake NoteManager that records posted notes."""

    def __init__(self) -> None:
        self.posted: list = []
        self.raise_on_submit_summary = False

    async def post(self, note) -> None:
        self.posted.append(note)

    async def submit_summary(
        self,
        *,
        task_id: str,
        agent_name: str,
        content: str,
        paths: list[str] | None = None,
        tags: list[str] | None = None,
    ):
        if self.raise_on_submit_summary:
            raise RuntimeError("summary store unavailable")
        from team.models import Note

        note = Note(
            id=f"summary-{len(self.posted)}",
            task_id=task_id,
            agent_name=agent_name,
            content=content,
            paths=list(paths or []),
            tags=["implementation", *(tags or [])],
        )
        self.posted.append(note)
        return note

    async def read(self, *, authors=None, tags=None) -> list:
        return []


class FakeQueue:
    """Fake TaskQueue that records enqueue calls."""

    def __init__(self) -> None:
        self.enqueued: list[str] = []

    def enqueue(self, task_id: str) -> None:
        self.enqueued.append(task_id)


def _make_handler(
    store: FakeStore,
    notes: FakeNoteManager,
    budget: FakeBudget,
    expander: FakeExpander,
    fail_fast: AsyncMock,
    cancel_event: asyncio.Event | None = None,
) -> TaskStatusHandler:
    handler = TaskStatusHandler(
        team_run_id="run-1",
        store=store,
        notes=notes,
        budget=budget,
        expander=expander,
        emit_event=lambda e: None,
        fail_fast=fail_fast,
        cancel_event=cancel_event,
    )
    return handler


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_success_marks_done_and_enqueues_newly_ready_deps():
    """DONE: dependent made READY by mark_done is pushed onto the queue."""
    dep_id = "dep-task"
    task_id = "main-task"

    store = FakeStore()
    task = _task(task_id, status=TaskStatus.RUNNING)
    dep = _task(dep_id, status=TaskStatus.READY)
    store.graph[task_id] = task
    store.graph[dep_id] = dep

    # mark_done returns the newly-ready dependent id
    store.mark_done.return_value = [dep_id]

    queue = FakeQueue()
    handler = _make_handler(store, FakeNoteManager(), FakeBudget(), FakeExpander(), AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(TaskStatusUpdate(task_id=task_id, status=TaskStatus.DONE, summary="ok"))

    store.mark_done.assert_awaited_once_with(task_id)
    assert dep_id in queue.enqueued


@pytest.mark.asyncio
async def test_success_on_parent_summary_sidecar_with_summary_finalizes_eas_parent(
    monkeypatch,
):
    """DONE on parent_summarizer sidecar → finalize_parent_summary called, note posted."""
    parent_id = "parent-task"
    sidecar_id = "sidecar-task"

    # Make get_role return "parent_summarizer" for the sidecar's agent
    monkeypatch.setattr("agents.registry.get_role", lambda name: "parent_summarizer")

    store = FakeStore()
    parent = _task(parent_id, status=TaskStatus.EXPANDED_AWAITING_SUMMARY, agent_name="developer")
    sidecar = _task(
        sidecar_id,
        status=TaskStatus.RUNNING,
        agent_name="parent_summarizer",
        fired_by_task_id=parent_id,
    )
    store.graph[parent_id] = parent
    store.graph[sidecar_id] = sidecar

    # mark_done returns nothing extra for the sidecar
    store.mark_done.return_value = []
    # get_record for parent must return EAS status
    store.get_record.return_value = SimpleNamespace(status="expanded_awaiting_summary")
    store.finalize_parent_summary.return_value = []

    notes = FakeNoteManager()
    queue = FakeQueue()
    handler = _make_handler(store, notes, FakeBudget(), FakeExpander(), AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=sidecar_id, status=TaskStatus.DONE, summary="roll-up")
    )

    store.finalize_parent_summary.assert_awaited_once_with(parent_id)
    parent_summary_notes = [
        n for n in notes.posted if "parent_summary" in getattr(n, "tags", [])
    ]
    assert parent_summary_notes, "expected a parent_summary-tagged note"
    assert parent_summary_notes[0].task_id == parent_id


@pytest.mark.asyncio
async def test_success_on_parent_summary_sidecar_empty_summary_fails_parent_and_fails_fast(
    monkeypatch,
):
    """DONE with empty summary → FAILED on parent + fail_fast called."""
    parent_id = "parent-task"
    sidecar_id = "sidecar-task"

    monkeypatch.setattr("agents.registry.get_role", lambda name: "parent_summarizer")

    store = FakeStore()
    parent = _task(parent_id, status=TaskStatus.EXPANDED_AWAITING_SUMMARY, agent_name="developer")
    sidecar = _task(
        sidecar_id,
        status=TaskStatus.RUNNING,
        agent_name="parent_summarizer",
        fired_by_task_id=parent_id,
    )
    store.graph[parent_id] = parent
    store.graph[sidecar_id] = sidecar
    store.mark_done.return_value = []

    cancel_event = asyncio.Event()
    fail_fast = AsyncMock()
    notes = FakeNoteManager()
    queue = FakeQueue()
    handler = _make_handler(store, notes, FakeBudget(), FakeExpander(), fail_fast, cancel_event)
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=sidecar_id, status=TaskStatus.DONE, summary="")
    )

    store.mark_failed.assert_awaited_once_with(parent_id, "parent_summary_empty")
    fail_fast.assert_awaited_once_with("parent_summary_empty")


@pytest.mark.asyncio
async def test_parent_summary_submit_failure_fails_parent_without_finalizing(
    monkeypatch,
):
    """Parent roll-up must be durable before the EAS parent can become DONE."""
    parent_id = "parent-task"
    sidecar_id = "sidecar-task"

    monkeypatch.setattr("agents.registry.get_role", lambda name: "parent_summarizer")

    store = FakeStore()
    store.graph[parent_id] = _task(
        parent_id,
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        agent_name="team_planner",
    )
    store.graph[sidecar_id] = _task(
        sidecar_id,
        status=TaskStatus.RUNNING,
        agent_name="parent_summarizer",
        fired_by_task_id=parent_id,
    )
    store.mark_done.return_value = []

    notes = FakeNoteManager()
    notes.raise_on_submit_summary = True
    fail_fast = AsyncMock()
    handler = _make_handler(store, notes, FakeBudget(), FakeExpander(), fail_fast)

    await handler.handle(
        TaskStatusUpdate(
            task_id=sidecar_id,
            status=TaskStatus.DONE,
            summary="roll-up",
        )
    )

    store.finalize_parent_summary.assert_not_awaited()
    store.mark_failed.assert_awaited_once_with(
        parent_id,
        "parent_summary_submit_failed",
    )
    fail_fast.assert_awaited_once_with("parent_summary_submit_failed")


@pytest.mark.asyncio
async def test_failed_marks_failed_and_calls_fail_fast_once():
    """FAILED: mark_failed called, fail_fast called once; idempotent on second handle."""
    task_id = "failing-task"

    store = FakeStore()
    store.graph[task_id] = _task(task_id, status=TaskStatus.RUNNING)

    cancel_event = asyncio.Event()
    fail_fast = AsyncMock()
    queue = FakeQueue()
    handler = _make_handler(store, FakeNoteManager(), FakeBudget(), FakeExpander(), fail_fast, cancel_event)
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=task_id, status=TaskStatus.FAILED, summary="boom")
    )

    store.mark_failed.assert_awaited_once_with(task_id, "boom")
    fail_fast.assert_awaited_once_with("boom")

    # Second call with cancel_event set — fail_fast must NOT be called again
    cancel_event.set()
    await handler.handle(
        TaskStatusUpdate(task_id=task_id, status=TaskStatus.FAILED, summary="boom")
    )

    assert fail_fast.await_count == 1


@pytest.mark.asyncio
async def test_expanded_with_plan_calls_mark_expanded_and_enqueues_ready_children():
    """EXPANDED: mark_expanded called; both READY children are enqueued."""
    planner_id = "planner-task"
    child_a = "child-a"
    child_b = "child-b"

    plan = Plan(
        tasks=[
            TaskDefinition(id=child_a, objective="task a", agent="developer"),
            TaskDefinition(id=child_b, objective="task b", agent="developer"),
        ]
    )

    child_task_a = _task(child_a, status=TaskStatus.READY)
    child_task_b = _task(child_b, status=TaskStatus.READY)

    store = FakeStore()
    store.graph[planner_id] = _task(planner_id, status=TaskStatus.RUNNING)
    store.graph[child_a] = child_task_a
    store.graph[child_b] = child_task_b
    # get_record needed by _on_expanded
    store.get_record.return_value = SimpleNamespace(id=planner_id)

    # expander returns the two children as new_items
    expander = FakeExpander(new_tasks=[child_task_a, child_task_b])

    queue = FakeQueue()
    handler = _make_handler(store, FakeNoteManager(), FakeBudget(), expander, AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=planner_id, status=TaskStatus.EXPANDED, plan=plan)
    )

    store.mark_expanded.assert_awaited_once_with(planner_id)
    assert child_a in queue.enqueued
    assert child_b in queue.enqueued


@pytest.mark.asyncio
async def test_request_replan_spawns_replanner_and_enqueues_it(monkeypatch):
    """REQUEST_REPLAN: request_replan called with reason+agent; replanner id enqueued."""
    task_id = "broken-task"
    replanner_id = "replanner-task-1"

    monkeypatch.setattr(
        "agents.registry.find_by_role",
        lambda role: [SimpleNamespace(name="replanner_agent")] if role == "replanner" else [],
    )

    replanner_task = _task(replanner_id, status=TaskStatus.READY)
    rec = SimpleNamespace(id=replanner_id)

    store = FakeStore()
    store.graph[task_id] = _task(task_id, status=TaskStatus.REQUEST_REPLAN)
    store.request_replan.return_value = (rec, True)

    # After request_replan, the replanner must be in graph for _enqueue to work
    async def _fake_request_replan(tid, *, reason, suggestion, replanner_agent):
        store.graph[replanner_id] = replanner_task
        return (rec, True)

    store.request_replan.side_effect = _fake_request_replan

    queue = FakeQueue()
    handler = _make_handler(store, FakeNoteManager(), FakeBudget(), FakeExpander(), AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=task_id, status=TaskStatus.REQUEST_REPLAN, summary="needs fixing")
    )

    store.request_replan.assert_awaited_once()
    call_kwargs = store.request_replan.call_args
    assert call_kwargs.kwargs.get("reason") == "needs fixing"
    assert call_kwargs.kwargs.get("replanner_agent") == "replanner_agent"
    assert replanner_id in queue.enqueued

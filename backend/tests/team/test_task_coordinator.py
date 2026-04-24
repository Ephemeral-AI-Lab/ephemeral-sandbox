"""Unit tests for TaskCoordinator core match-block cases."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from team.core.models import (
    BudgetConfig,
    BudgetState,
    LeafSubmission,
    Plan,
    PlannerSubmission,
    SubmittedSummary,
    Task,
    TaskDefinition,
    TaskStatus,
    TaskStatusUpdate,
)
from team.runtime.task_coordinator import TaskCoordinator


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
    spec = {
        "goal": "do something",
        "detail": "Do the assigned work.",
        "acceptance_criteria": "Submit the terminal outcome.",
    }
    return Task(
        id=task_id,
        team_run_id="run-1",
        definition=TaskDefinition(id=task_id, spec=spec, agent=agent_name),
        status=status,
        fired_by_task_id=fired_by_task_id,
    )


class FakeStore:
    """In-memory fake that satisfies TaskCoordinator's store interface."""

    def __init__(self) -> None:
        self.graph: dict[str, Task] = {}

        # Async methods with configurable return values
        self.mark_done = AsyncMock(return_value=[])
        self.mark_expanded = AsyncMock()
        self.mark_failed = AsyncMock()
        self.mark_terminal = AsyncMock()
        self.refresh_graph = AsyncMock()
        self.get_record = AsyncMock(return_value=None)
        self.finalize_replanned_origin = AsyncMock(return_value=None)
        self.request_replan = AsyncMock()
        self.cascade_cancel_recursive = AsyncMock()
        self.fetch_promotable_parent = AsyncMock(return_value=None)

    def get_task(self, task_id: str) -> Task | None:
        return self.graph.get(task_id)

    def terminal_child_ids(self) -> list[str]:
        return [
            task.id
            for task in self.graph.values()
            if task.parent_id is not None and task.status in {
                TaskStatus.DONE,
                TaskStatus.FAILED,
                TaskStatus.CANCELLED,
                TaskStatus.REQUEST_REPLAN,
            }
        ]


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


class FakeQueue:
    """Fake TaskQueue that records enqueue calls."""

    def __init__(self) -> None:
        self.enqueued: list[str] = []

    def enqueue(self, task_id: str) -> None:
        self.enqueued.append(task_id)


def _make_handler(
    store: FakeStore,
    budget: FakeBudget,
    expander: FakeExpander,
    fail_fast: AsyncMock,
    cancel_event: asyncio.Event | None = None,
) -> TaskCoordinator:
    handler = TaskCoordinator(
        team_run_id="run-1",
        store=store,
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
    handler = _make_handler(store, FakeBudget(), FakeExpander(), AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(TaskStatusUpdate(task_id=task_id, status=TaskStatus.DONE, summary="ok"))

    store.mark_done.assert_awaited_once_with(task_id)
    assert dep_id in queue.enqueued


@pytest.mark.asyncio
async def test_success_promotes_parent_with_synthesized_child_summary(
    monkeypatch,
):
    """DONE child of an expanded parent -> synthesized parent summary + DONE."""
    parent_id = "parent-task"
    child_id = "child-task"

    monkeypatch.setattr("agents.registry.has_role", lambda name, role: False)

    store = FakeStore()
    parent = _task(parent_id, status=TaskStatus.EXPANDED, agent_name="team_planner")
    parent.submission = PlannerSubmission(plan=Plan())
    child = _task(
        child_id,
        status=TaskStatus.RUNNING,
        agent_name="developer",
    )
    child.parent_id = parent_id
    store.graph[parent_id] = parent
    store.graph[child_id] = child

    store.mark_done.return_value = []
    store.fetch_promotable_parent.side_effect = [parent_id, None]

    queue = FakeQueue()
    handler = _make_handler(store, FakeBudget(), FakeExpander(), AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=child_id, status=TaskStatus.DONE, summary="child delivered")
    )

    assert store.mark_done.await_args_list[0].args == (child_id,)
    assert store.mark_done.await_args_list[1].args == (parent_id,)
    assert parent.submission.summary is not None
    assert parent.submission.summary.summary == "child delivered"


@pytest.mark.asyncio
async def test_synthesized_parent_summary_prefers_terminal_validator(
    monkeypatch,
):
    """Parent summary uses the terminal validator when one exists."""
    parent_id = "parent-task"
    dev_id = "dev-task"
    validator_id = "validator-task"

    monkeypatch.setattr(
        "agents.registry.has_role",
        lambda name, role: name == "validator" and role == "reviewer",
    )

    store = FakeStore()
    parent = _task(parent_id, status=TaskStatus.EXPANDED, agent_name="team_planner")
    dev = _task(dev_id, status=TaskStatus.DONE, agent_name="developer")
    dev.parent_id = parent_id
    dev.submission = LeafSubmission(summary=SubmittedSummary(summary="developer summary"))
    validator = _task(validator_id, status=TaskStatus.RUNNING, agent_name="validator")
    validator.parent_id = parent_id
    validator.submission = LeafSubmission(summary=SubmittedSummary(summary="validator summary"))
    store.graph[parent_id] = parent
    store.graph[dev_id] = dev
    store.graph[validator_id] = validator
    store.mark_done.return_value = []
    store.fetch_promotable_parent.side_effect = [parent_id, None]

    cancel_event = asyncio.Event()
    fail_fast = AsyncMock()
    queue = FakeQueue()
    handler = _make_handler(store, FakeBudget(), FakeExpander(), fail_fast, cancel_event)
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=validator_id, status=TaskStatus.DONE, summary="validator summary")
    )

    store.mark_failed.assert_not_awaited()
    fail_fast.assert_not_awaited()
    assert parent.submission is not None
    assert isinstance(parent.submission, PlannerSubmission)
    assert parent.submission.summary is not None
    assert parent.submission.summary.summary == "validator summary"


@pytest.mark.asyncio
async def test_failed_marks_failed_and_calls_fail_fast_once():
    """FAILED: mark_failed called, fail_fast called once; idempotent on second handle."""
    task_id = "failing-task"

    store = FakeStore()
    store.graph[task_id] = _task(task_id, status=TaskStatus.RUNNING)

    cancel_event = asyncio.Event()
    fail_fast = AsyncMock()
    queue = FakeQueue()
    handler = _make_handler(store, FakeBudget(), FakeExpander(), fail_fast, cancel_event)
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
            TaskDefinition(
                id=child_a,
                spec={
                    "goal": "task a",
                    "detail": "Do task a.",
                    "acceptance_criteria": "Submit the terminal outcome.",
                },
                agent="developer",
            ),
            TaskDefinition(
                id=child_b,
                spec={
                    "goal": "task b",
                    "detail": "Do task b.",
                    "acceptance_criteria": "Submit the terminal outcome.",
                },
                agent="developer",
            ),
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
    handler = _make_handler(store, FakeBudget(), expander, AsyncMock())
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
    handler = _make_handler(store, FakeBudget(), FakeExpander(), AsyncMock())
    handler.bind_queue(queue)

    await handler.handle(
        TaskStatusUpdate(task_id=task_id, status=TaskStatus.REQUEST_REPLAN, summary="needs fixing")
    )

    store.request_replan.assert_awaited_once()
    call_kwargs = store.request_replan.call_args
    assert call_kwargs.kwargs.get("reason") == "needs fixing"
    assert call_kwargs.kwargs.get("replanner_agent") == "replanner_agent"
    assert replanner_id in queue.enqueued

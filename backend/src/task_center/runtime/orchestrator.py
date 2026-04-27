"""TaskCenter — request-scoped orchestrator for the GAN-style task graph.

Each user query routes through a fresh ``TaskCenter.run_query``. The class owns:

- :class:`TaskGraph` — the in-memory task + harness-graph store
- the five mode-tool entry points (called from ``tools.mode_tool``)
- a wakeup event that the submission methods set after every state change
- a dispatcher loop that spawns one agent coroutine per ready task

Pure read-only queries over the graph live in :mod:`task_center.graph.queries`
and :mod:`task_center.graph.readiness`. Role-specific lifecycle operations live
under :mod:`task_center.harness_agents`. Thin method wrappers on ``TaskCenter``
delegate to those free functions so callers can keep using
``tc.dependency_blocked_descendants(...)``-style attribute access and the
public mode-tool entry points.
"""

from __future__ import annotations

import asyncio
import itertools
import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any

from task_center.errors import TaskCenterError
from task_center.graph import (
    TaskGraph,
    dependency_blocked_descendants as _q_dependency_blocked_descendants,
    is_harness_graph_ready_for_evaluation as _q_is_harness_graph_ready_for_evaluation,
)
from task_center.harness_agents.evaluator import lifecycle as evaluator_lifecycle
from task_center.harness_agents.executor import lifecycle as executor_lifecycle
from task_center.harness_agents.planner import lifecycle as planner_lifecycle
from task_center.model import (
    HarnessGraph,
    HarnessGraphId,
    Status,
    Task,
    TaskId,
)

if TYPE_CHECKING:
    from db.stores.task_center_store import TaskCenterStore

logger = logging.getLogger(__name__)


SpawnFunc = Callable[[TaskId, "TaskCenter", str | None], Awaitable[None]]


_TERMINAL_STATUSES: frozenset[Status] = frozenset({Status.DONE, Status.FAILED})


class TaskCenter:
    """Request-scoped orchestrator created by the server runtime."""

    def __init__(
        self,
        runtime_config: Any = None,
        *,
        spawn_func: SpawnFunc | None = None,
        id_prefix: str = "t",
        on_event: "Callable[[Any], Awaitable[None]] | None" = None,
        request_id: str | None = None,
        run_id: str | None = None,
        task_center_store: "TaskCenterStore | None" = None,
    ) -> None:
        self._graph: TaskGraph = TaskGraph()
        del runtime_config
        self._spawn_func: SpawnFunc | None = spawn_func
        self._wakeup: asyncio.Event = asyncio.Event()
        self._counter = itertools.count(1)
        self._graph_counter = itertools.count(1)
        self._id_prefix = id_prefix
        self._on_event: "Callable[[Any], Awaitable[None]] | None" = on_event
        self.request_id = request_id
        self.run_id = run_id
        self._task_center_store = task_center_store

    def set_event_callback(self, on_event: "Callable[[Any], Awaitable[None]] | None") -> None:
        self._on_event = on_event

    async def _emit_event(self, event: Any) -> None:
        if self._on_event is not None:
            await self._on_event(event)

    @property
    def graph(self) -> TaskGraph:
        return self._graph

    def _new_id(self) -> TaskId:
        return f"{self._id_prefix}{next(self._counter)}"

    def _new_graph_id(self) -> HarnessGraphId:
        return f"g{next(self._graph_counter)}"

    def persisted_task_id(self, task_id: TaskId) -> str:
        if self.run_id is None:
            return task_id
        return f"{self.run_id}:{task_id}"

    def persisted_graph_id(self, graph_id: HarnessGraphId) -> str:
        if self.run_id is None:
            return graph_id
        return f"{self.run_id}:{graph_id}"

    def _persist_task(self, task: Task) -> None:
        if self._task_center_store is None or self.run_id is None:
            return
        persisted_id = self.persisted_task_id(task.id)
        self._task_center_store.upsert_task(
            task_id=persisted_id,
            run_id=self.run_id,
            role=task.role,
            task_input=task.input,
            status=task.status.value,
            summaries=[
                {
                    "kind": s.kind,
                    "text": s.text,
                    "source_task_id": self.persisted_task_id(s.source_task_id),
                    "created_at": s.created_at,
                }
                for s in task.summaries
            ],
            needs=[self.persisted_task_id(n) for n in sorted(task.needs)],
            task_center_harness_graph_id=(
                self.persisted_graph_id(task.task_center_harness_graph_id)
                if task.task_center_harness_graph_id is not None
                else None
            ),
        )

    def _persist_harness_graph(self, graph: HarnessGraph) -> None:
        if self._task_center_store is None or self.run_id is None:
            return
        self._task_center_store.upsert_harness_graph(
            graph_id=self.persisted_graph_id(graph.id),
            run_id=self.run_id,
            parent_task_id=self.persisted_task_id(graph.parent_task_id),
            planner_task_id=self.persisted_task_id(graph.planner_task_id),
            evaluator_task_id=(
                self.persisted_task_id(graph.evaluator_task_id)
                if graph.evaluator_task_id is not None
                else None
            ),
            executor_task_ids=[
                self.persisted_task_id(eid) for eid in graph.executor_task_ids
            ],
        )

    def _persist_all(self) -> None:
        for task in self._graph.tasks.values():
            self._persist_task(task)
        for graph in self._graph.harness_graphs.values():
            self._persist_harness_graph(graph)

    def _finish_persisted_run(self, status: str) -> None:
        if self._task_center_store is None or self.run_id is None:
            return
        self._task_center_store.finish_run(self.run_id, status)

    # ------------------------------------------------------------------ #
    # Root creation                                                      #
    # ------------------------------------------------------------------ #

    def _create_root_executor(self, prompt: str) -> Task:
        return executor_lifecycle.create_root_executor(self, prompt)

    # ------------------------------------------------------------------ #
    # Graph queries — thin wrappers over task_center.graph.queries       #
    # ------------------------------------------------------------------ #

    def dependency_blocked_descendants(self, task_id: TaskId) -> list[Task]:
        return _q_dependency_blocked_descendants(self._graph, task_id)

    def is_harness_graph_ready_for_evaluation(self, graph_id: HarnessGraphId) -> bool:
        return _q_is_harness_graph_ready_for_evaluation(self._graph, graph_id)

    # ------------------------------------------------------------------ #
    # Mode-tool entry points                                             #
    # ------------------------------------------------------------------ #

    def submit_task_success(self, task_id: TaskId, summary: str) -> None:
        task = self._graph.get(task_id)
        if task.role not in ("executor", "evaluator"):
            raise TaskCenterError(
                f"submit_task_success: task {task_id!r} role {task.role!r} not allowed"
            )
        if task.role == "executor":
            executor_lifecycle.submit_task_success(self, task_id, summary)
        else:
            evaluator_lifecycle.submit_task_success(self, task_id, summary)

    def submit_task_failure(self, task_id: TaskId, summary: str) -> None:
        executor_lifecycle.submit_task_failure(self, task_id, summary)

    def submit_evaluation_failure(self, task_id: TaskId, summary: str) -> None:
        evaluator_lifecycle.submit_evaluation_failure(self, task_id, summary)

    def request_plan(self, task_id: TaskId, request_plan_note: str) -> None:
        planner_lifecycle.request_plan(self, task_id, request_plan_note)

    def submit_plan_handoff(
        self,
        planner_id: TaskId,
        tasks: list[dict[str, Any]],
        task_inputs: dict[str, str],
        handoff_plan_note: str,
        evaluator_note: str,
    ) -> None:
        planner_lifecycle.submit_plan_handoff(
            self,
            planner_id,
            tasks,
            task_inputs,
            handoff_plan_note,
            evaluator_note,
        )

    def _notify_child_terminal_changed(self) -> None:
        # The dispatcher polls is_harness_graph_ready_for_evaluation each tick,
        # so it picks up the evaluator promotion. Just wake the loop here.
        self._wakeup.set()

    def _mark_terminal(self, task: Task, terminal: Status) -> None:
        if task.status is terminal:
            return
        self._graph.transition(task.id, terminal)

    # ------------------------------------------------------------------ #
    # Dispatcher                                                         #
    # ------------------------------------------------------------------ #

    async def run_query(self, prompt: str, *, sandbox_id: str | None = None) -> Task:
        if self._spawn_func is None:
            raise TaskCenterError(
                "TaskCenter.run_query requires a spawn_func — pass one to "
                "the constructor."
            )

        self._graph = TaskGraph()
        root = self._create_root_executor(prompt)
        running: dict[TaskId, asyncio.Task[None]] = {}

        def _promote_ready_evaluators() -> None:
            for graph in self._graph.harness_graphs.values():
                if graph.evaluator_task_id is None:
                    continue
                evaluator = self._graph.get(graph.evaluator_task_id)
                if evaluator.status is not Status.PENDING:
                    continue
                if self.is_harness_graph_ready_for_evaluation(graph.id):
                    self._graph.transition(evaluator.id, Status.READY)
                    self._persist_task(evaluator)

        def _spawn_for_ready() -> None:
            _promote_ready_evaluators()
            for task in self._graph.ready_tasks():
                if task.id in running:
                    continue
                if task.status is Status.PENDING:
                    self._graph.transition(task.id, Status.READY)
                    self._persist_task(task)
                self._graph.transition(task.id, Status.RUNNING)
                self._persist_task(task)
                coro = self._run_one(task.id, sandbox_id)
                running[task.id] = asyncio.create_task(coro)

        final_status = "cancelled"
        try:
            _spawn_for_ready()
            while self._graph.get(root.id).status not in _TERMINAL_STATUSES:
                wakeup_task = asyncio.create_task(self._wakeup.wait())
                await asyncio.wait(
                    [wakeup_task, *list(running.values())],
                    return_when=asyncio.FIRST_COMPLETED,
                )
                if not wakeup_task.done():
                    wakeup_task.cancel()
                self._wakeup.clear()
                for tid, t in list(running.items()):
                    if t.done():
                        running.pop(tid)
                _spawn_for_ready()
            final_status = self._graph.get(root.id).status.value
        finally:
            for t in running.values():
                if not t.done():
                    t.cancel()
            self._persist_all()
            self._finish_persisted_run(final_status)

        return self._graph.get(root.id)

    async def _run_one(
        self,
        task_id: TaskId,
        sandbox_id: str | None,
    ) -> None:
        assert self._spawn_func is not None
        try:
            await self._spawn_func(task_id, self, sandbox_id)
        except Exception:
            logger.exception("agent for task %r crashed", task_id)
            task = self._graph.get(task_id)
            if task.status is Status.RUNNING:
                self._handle_silent_termination(task, "agent crashed")
            return
        task = self._graph.get(task_id)
        if task.status is Status.RUNNING:
            self._handle_silent_termination(
                task, "agent exited without a terminal tool call"
            )

    def _handle_silent_termination(self, task: Task, reason: str) -> None:
        """Treat a silent agent exit as a role-appropriate terminal."""
        if task.role == "executor":
            executor_lifecycle.handle_silent_termination(self, task, reason)
        elif task.role == "planner":
            planner_lifecycle.handle_silent_termination(self, task, reason)
        else:
            evaluator_lifecycle.handle_silent_termination(self, task, reason)

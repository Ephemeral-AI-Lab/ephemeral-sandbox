"""TaskCenter — request-scoped orchestrator for the executor-evaluator tree.

Each user query routes through a fresh ``TaskCenter.run_query``. The class owns:

- :class:`TaskGraph` — the in-memory task tree
- the four mode-tool entry points (called from ``tools.mode_tool``)
- a wakeup event that the submission methods set after every state change
- a dispatcher loop that spawns one agent coroutine per ready task
"""

from __future__ import annotations

import asyncio
import itertools
import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any

from task_center.dag import compile_dag
from task_center.errors import TaskCenterError
from task_center.graph import TaskGraph
from task_center.propagation import close_with_summary
from task_center.task import Status, Task, TaskId

if TYPE_CHECKING:
    from db.stores.task_center_store import TaskCenterStore

logger = logging.getLogger(__name__)


# A spawn function takes a task_id, a TaskCenter, and the request sandbox id,
# runs the agent for that task, and returns when the agent loop exits. Tests
# inject a scripted coroutine so the dispatcher can be exercised without a
# real LLM.
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
        self._runtime_config = runtime_config
        self._spawn_func: SpawnFunc | None = spawn_func
        self._wakeup: asyncio.Event = asyncio.Event()
        self._counter = itertools.count(1)
        self._id_prefix = id_prefix
        self._on_event: "Callable[[Any], Awaitable[None]] | None" = on_event
        self.request_id = request_id
        self.run_id = run_id
        self._task_center_store = task_center_store

    def set_event_callback(self, on_event: "Callable[[Any], Awaitable[None]] | None") -> None:
        """Replace the event callback. Each /chat invocation sets its own."""
        self._on_event = on_event

    async def _emit_event(self, event: Any) -> None:
        if self._on_event is not None:
            await self._on_event(event)

    # ------------------------------------------------------------------ #
    # Public surface                                                     #
    # ------------------------------------------------------------------ #

    @property
    def graph(self) -> TaskGraph:
        """Read-mostly access to the task graph."""
        return self._graph

    def _new_id(self) -> TaskId:
        return f"{self._id_prefix}{next(self._counter)}"

    def persisted_task_id(self, task_id: TaskId) -> str:
        """Return the persisted task id for an in-memory TaskCenter id."""
        if self.run_id is None:
            return task_id
        return f"{self.run_id}:{task_id}"

    def _persist_task(self, task: Task) -> None:
        if self._task_center_store is None or self.run_id is None:
            return
        persisted_id = self.persisted_task_id(task.id)
        self._task_center_store.upsert_task(
            task_id=persisted_id,
            run_id=self.run_id,
            role=task.role,
            title=task.title,
            task_input=task.spec,
            status=task.status.value,
            summary=task.summary,
        )
        self._task_center_store.upsert_graph_node(
            run_id=self.run_id,
            task_id=persisted_id,
            parent_task_id=(
                self.persisted_task_id(task.parent_id)
                if task.parent_id is not None
                else None
            ),
            children_ids=[self.persisted_task_id(child_id) for child_id in task.children],
            evaluator_id=(
                self.persisted_task_id(task.evaluator_id)
                if task.evaluator_id is not None
                else None
            ),
            acceptance_criteria=task.acceptance_criteria,
            handoff_note=task.handoff_note,
        )

    def _persist_tasks(self, *tasks: Task) -> None:
        for task in tasks:
            self._persist_task(task)

    def _persist_all_tasks(self) -> None:
        for task in self._graph.tasks.values():
            self._persist_task(task)

    def _finish_persisted_run(self, status: str) -> None:
        if self._task_center_store is None or self.run_id is None:
            return
        self._task_center_store.finish_run(self.run_id, status)

    # ------------------------------------------------------------------ #
    # Root creation                                                      #
    # ------------------------------------------------------------------ #

    def _create_root_executor(self, prompt: str) -> Task:
        task = Task(
            id=self._new_id(),
            role="executor",
            title="Root",
            spec=prompt,
            status=Status.READY,
            closes_for=None,
        )
        self._graph.add(task)
        if self._task_center_store is not None and self.run_id is not None:
            self._task_center_store.set_run_root(self.run_id, self.persisted_task_id(task.id))
        self._persist_task(task)
        return task

    # ------------------------------------------------------------------ #
    # Mode tool entry points (called from mode tools)             #
    # ------------------------------------------------------------------ #

    def submit_task_completion(self, task_id: TaskId, summary: str) -> None:
        """Close ``task_id`` with ``summary`` and propagate up the closes_for chain."""
        # close_with_summary writes status=DONE directly (bypassing transition
        # guards) — required because waiting states -> DONE only happens via
        # propagation, not via the transition() method (invariant 14).
        close_with_summary(self._graph.tasks, task_id, summary)
        self._persist_all_tasks()
        self._wakeup.set()

    def submit_plan_handoff(
        self,
        executor_id: TaskId,
        tasks: list[dict[str, Any]],
        task_specs: dict[str, dict[str, Any]],
        acceptance_criteria: str,
        handoff_note: str,
    ) -> None:
        """Validate plan, materialize child executors, mark parent HANDOFF.

        Every handoff carries a ``handoff_note`` — the evaluator validates
        children against ``acceptance_criteria`` regardless, and the note is
        forwarded so it can read the executor's articulation of risks before
        deciding. The evaluator itself is NOT created here; it is materialized
        by the dispatcher only after every child executor reaches DONE.
        """
        deps = compile_dag(tasks, task_specs)

        parent = self._graph.get(executor_id)
        parent.acceptance_criteria = acceptance_criteria
        parent.handoff_note = handoff_note

        # Materialize child executor tasks. Tasks with no deps are READY
        # immediately; the rest stay PENDING until their direct deps are DONE.
        for entry in tasks:
            tid = entry["id"]
            task_spec = task_specs[tid]
            task_input = task_spec.get("task_input")
            if not isinstance(task_input, str) or not task_input:
                raise TaskCenterError(
                    f"submit_plan_handoff: task_specs[{tid!r}] missing task_input"
                )
            child_status = Status.READY if not deps[tid] else Status.PENDING
            child = Task(
                id=tid,
                role="executor",
                title=task_spec["title"],
                spec=task_input,
                status=child_status,
                parent_id=executor_id,
                needs=deps[tid],
                closes_for=None,
            )
            self._graph.add(child)
            parent.children.append(tid)
            self._persist_task(child)

        # Parent transitions RUNNING -> HANDOFF while child executors run.
        self._graph.transition(executor_id, Status.HANDOFF)
        self._persist_task(parent)
        self._wakeup.set()

    def submit_continue_work_handoff(self, evaluator_id: TaskId, task_input: str) -> None:
        """Spawn a continuation executor under the evaluator; evaluator -> HANDOFF."""
        evaluator = self._graph.get(evaluator_id)
        if evaluator.role != "evaluator":
            raise TaskCenterError(
                f"submit_continue_work_handoff: task {evaluator_id!r} is not an evaluator"
            )

        cont_id = self._new_id()
        cont = Task(
            id=cont_id,
            role="executor",
            title=f"Continuation under {evaluator_id}",
            spec=(
                "Continue the parent task and address the evaluator's gap.\n\n"
                f"Continuation input:\n{task_input}"
            ),
            status=Status.READY,
            parent_id=evaluator_id,
            closes_for=evaluator_id,
            acceptance_criteria=evaluator.acceptance_criteria,
        )
        self._graph.add(cont)
        evaluator.children.append(cont_id)
        self._persist_task(cont)

        # Evaluator was RUNNING; now HANDOFF until continuation closure.
        self._graph.transition(evaluator_id, Status.HANDOFF)
        self._persist_task(evaluator)
        self._wakeup.set()

    # ------------------------------------------------------------------ #
    # Materialize evaluator after all handoff children are DONE          #
    # ------------------------------------------------------------------ #

    def _materialize_pending_evaluators(self) -> None:
        """Spawn a READY evaluator for any handoff executor whose children all DONE.

        Handoff submissions create only child executors. Once every child
        reaches DONE, the parent still sits in HANDOFF with
        ``evaluator_id is None`` — that's the signal to create its evaluator.
        """
        for parent in list(self._graph.tasks.values()):
            if parent.role != "executor":
                continue
            if parent.status is not Status.HANDOFF:
                continue
            if parent.evaluator_id is not None:
                continue
            if not all(
                self._graph.get(child_id).status is Status.DONE
                for child_id in parent.children
            ):
                continue

            eval_id = f"{parent.id}-eval"
            evaluator = Task(
                id=eval_id,
                role="evaluator",
                title=f"Evaluator for {parent.id}",
                spec=(
                    "Validate the parent task's acceptance_criteria against direct "
                    "child summaries."
                ),
                status=Status.READY,
                parent_id=parent.id,
                closes_for=parent.id,
                acceptance_criteria=parent.acceptance_criteria,
                handoff_note=parent.handoff_note,
            )
            self._graph.add(evaluator)
            parent.children.append(eval_id)
            parent.evaluator_id = eval_id
            self._persist_tasks(parent, evaluator)

    # ------------------------------------------------------------------ #
    # Dispatcher loop                                                    #
    # ------------------------------------------------------------------ #

    async def run_query(self, prompt: str, *, sandbox_id: str | None = None) -> Task:
        """Drive a user query end-to-end. Returns the closed root task.

        Spawns one ``asyncio.Task`` per ready task. Each spawn calls
        ``self._spawn_func(task_id, self, sandbox_id)``. The loop exits when
        the root task's status is DONE or FAILED.
        """
        if self._spawn_func is None:
            raise TaskCenterError(
                "TaskCenter.run_query requires a spawn_func — pass one to "
                "the constructor (or wire production spawn in US-009)."
            )

        self._graph = TaskGraph()
        root = self._create_root_executor(prompt)
        running: dict[TaskId, asyncio.Task[None]] = {}

        def _spawn_for_ready() -> None:
            self._materialize_pending_evaluators()
            for task in self._graph.ready_tasks():
                if task.id in running:
                    continue
                if task.status is Status.PENDING:
                    self._graph.transition(task.id, Status.READY)
                    self._persist_task(task)
                self._graph.transition(task.id, Status.RUNNING)
                self._persist_task(task)
                coro = self._run_one(task.id, root.id, sandbox_id)
                running[task.id] = asyncio.create_task(coro)

        final_status = "cancelled"
        try:
            _spawn_for_ready()
            while self._graph.get(root.id).status not in _TERMINAL_STATUSES:
                # Wait for a state change OR for any agent to finish.
                wakeup_task = asyncio.create_task(self._wakeup.wait())
                done, pending = await asyncio.wait(
                    [wakeup_task, *list(running.values())],
                    return_when=asyncio.FIRST_COMPLETED,
                )
                # Always cancel the wakeup waiter so it doesn't leak.
                if not wakeup_task.done():
                    wakeup_task.cancel()
                self._wakeup.clear()
                # Drop completed agent tasks from `running`.
                for tid, t in list(running.items()):
                    if t.done():
                        running.pop(tid)
                _spawn_for_ready()
            final_status = self._graph.get(root.id).status.value
        finally:
            # Cancel any still-running agents on exit.
            for t in running.values():
                if not t.done():
                    t.cancel()
            self._persist_all_tasks()
            self._finish_persisted_run(final_status)

        return self._graph.get(root.id)

    def _fail_team_run(
        self,
        root_id: TaskId,
        failed_task_id: TaskId,
        reason: str,
    ) -> None:
        """Fail the active root when any task in its tree fails."""
        root = self._graph.get(root_id)
        if root.status in _TERMINAL_STATUSES:
            return
        if root.id == failed_task_id:
            root.summary = reason
        else:
            root.summary = f"team run failed because task {failed_task_id!r} failed: {reason}"
        self._graph.transition(root_id, Status.FAILED)
        self._persist_task(root)
        self._wakeup.set()

    async def _run_one(
        self,
        task_id: TaskId,
        root_id: TaskId,
        sandbox_id: str | None,
    ) -> None:
        """Run one agent. Mark FAILED if it returns without a terminal."""
        assert self._spawn_func is not None
        try:
            await self._spawn_func(task_id, self, sandbox_id)
        except Exception:
            logger.exception("agent for task %r crashed", task_id)
            task = self._graph.get(task_id)
            if task.status is Status.RUNNING:
                self._graph.transition(task.id, Status.FAILED)
                task.summary = "agent crashed"
                self._persist_task(task)
            self._fail_team_run(root_id, task_id, "agent crashed")
            return
        # If the agent returned without calling a terminal tool, mark FAILED.
        task = self._graph.get(task_id)
        if task.status is Status.RUNNING:
            self._graph.transition(task.id, Status.FAILED)
            task.summary = "agent exited without a terminal tool call"
            self._persist_task(task)
            self._fail_team_run(root_id, task_id, task.summary)

"""TeamRun lifecycle container."""

from __future__ import annotations

import asyncio
import logging
import os
import uuid
from dataclasses import asdict
from typing import Any, Callable

from team.core.models import BudgetConfig, BudgetState, TeamRunStatus
from team.persistence.events import make_team_run_created, make_team_run_status
from team.persistence.run_store import TeamRunStore
from team.runtime.executor import Executor
from team.runtime.run_registry import register as _register_team_run
from team.runtime.run_registry import unregister as _unregister_team_run
from team.runtime.services import TeamRuntimeServices, build_team_runtime_services
from team.runtime.status_handler import TaskStatusHandler
from team.runtime.task_queue import TaskQueue


def _default_num_executors() -> int:
    raw = os.getenv("TEAM_DEFAULT_NUM_EXECUTORS", "2").strip()
    try:
        return max(1, int(raw))
    except ValueError:
        return 2


logger = logging.getLogger(__name__)


class TeamRun:
    def __init__(
        self,
        *,
        session_id: str,
        user_request: str,
        budgets: BudgetConfig | None = None,
        goal: str | None = None,
        sandbox_id: str | None = None,
        repo_root: str | None = None,
        team_run_id: str | None = None,
        event_store: TeamRunStore | None = None,
        services: TeamRuntimeServices | None = None,
    ) -> None:
        self.id = team_run_id or str(uuid.uuid4())
        self.session_id = session_id
        self.user_request = user_request
        self.sandbox_id = sandbox_id
        self.budgets = budgets or BudgetConfig()
        self.budget_state = BudgetState()
        self.status = TeamRunStatus.PENDING
        self._fatal_failure_reason: str | None = None
        runtime_services = services or build_team_runtime_services(
            team_run_id=self.id,
            budgets=self.budgets,
            budget_state=self.budget_state,
            user_request=user_request,
            goal=goal,
            repo_root=repo_root,
            event_store=event_store,
        )
        self._active_agent_runs: dict[str, asyncio.Task[object]] = {}
        self.task_center = runtime_services.task_center
        self.budgets = self.task_center.budgets
        self.budget_state = self.task_center.budget_state
        self.project_context = runtime_services.project_context
        self.event_store: TeamRunStore = getattr(
            runtime_services, "event_store", TeamRunStore()
        )
        self.cancel_event = asyncio.Event()
        self.root_task_id: str | None = None
        self._num_executors: int = _default_num_executors()
        self.coordination_metadata: dict[str, Any] = {}
        self.roster: dict[str, list[str]] = {}
        self.team_definition: Any | None = None
        self.arbiter: Any = getattr(runtime_services, "arbiter", None)
        self.status_handler = TaskStatusHandler(
            team_run_id=self.id,
            store=self.task_center.store,
            budget=self.task_center.budget,
            expander=self.task_center.expander,
            emit_event=self.task_center.emit_event,
            fail_fast=self.fail_fast,
            cancel_running_task=self.cancel_running_task,
            cancel_event=self.cancel_event,
            roster_getter=lambda: self.roster,
        )
        self.task_queue: TaskQueue | None = None

    # ---- live agent run registry ----------------------------------------

    def register_agent_run(self, task_id: str, runner_task: asyncio.Task[object]) -> None:
        self._active_agent_runs[task_id] = runner_task

    def unregister_agent_run(self, task_id: str, runner_task: asyncio.Task[object]) -> None:
        current = self._active_agent_runs.get(task_id)
        if current is runner_task:
            self._active_agent_runs.pop(task_id, None)

    def cancel_running_task(self, task_id: str) -> None:
        runner_task = self._active_agent_runs.get(task_id)
        if runner_task is None or runner_task.done():
            return
        runner_task.cancel()

    # ---- lifecycle ------------------------------------------------------

    async def start(
        self,
        agent_name: str,
        payload: dict[str, Any],
        *,
        executor_factory: Callable[["TeamRun"], Executor],
        num_executors: int | None = None,
    ) -> None:
        from team.core.models import Task, TaskDefinition, TaskStatus

        spec = payload.get("spec")
        if spec is None:
            raise ValueError("Root payload requires a non-empty 'spec'")
        root_id = str(uuid.uuid4())
        root = Task(
            id=root_id,
            team_run_id=self.id,
            definition=TaskDefinition(
                id=root_id,
                spec=spec,
                agent=agent_name,
                scope_paths=list(payload.get("scope_paths", [])),
            ),
            status=TaskStatus.PENDING,
            depth=0,
        )
        root.root_id = root.id
        self.root_task_id = root.id
        self.event_store.append(
            make_team_run_created(
                self.id,
                session_id=self.session_id,
                user_request=self.user_request,
                goal=None,
                repo_root=self.project_context.repo_root,
                sandbox_id=self.sandbox_id,
                budgets=asdict(self.budgets),
                roster=dict(self.roster) if self.roster else None,
            )
        )
        self.status = TeamRunStatus.RUNNING
        self.event_store.append(make_team_run_status(self.id, self.status.value))
        _register_team_run(self)

        if num_executors is not None:
            self._num_executors = max(1, int(num_executors))
        executor = executor_factory(self)
        self.task_queue = TaskQueue(
            num_workers=self._num_executors,
            executor=executor,
            handler=self.status_handler,
        )
        self.status_handler.bind_queue(self.task_queue)
        await self.task_queue.start()
        # Restart recovery must happen after the queue is bound so any
        # re-injected sidecars land on the push queue.
        await self.status_handler.recover_awaiting_summary_parents()
        await self.task_center.add_task(root)
        await self.status_handler.on_task_added(root)

    async def start_with_team_definition(
        self,
        team_def: Any,
        payload: dict[str, Any],
        *,
        executor_factory: Callable[["TeamRun"], Executor],
        num_executors: int | None = None,
    ) -> None:
        from agents.registry import get_definition

        if get_definition(team_def.entry_planner) is None:
            raise ValueError(
                f"team_definition '{team_def.name}' entry_planner "
                f"'{team_def.entry_planner}' does not exist"
            )
        self.team_definition = team_def
        self.roster = dict(team_def.roster)
        await self.start(
            agent_name=team_def.entry_planner,
            payload=payload,
            executor_factory=executor_factory,
            num_executors=num_executors,
        )

    async def _is_all_terminal(self) -> bool:
        return await self.task_center.store.all_terminal()

    async def wait(self, *, timeout: float | None = None) -> TeamRunStatus:
        try:
            elapsed = 0.0
            while not await self._is_all_terminal():
                queue = self.task_queue
                if queue is not None and queue.workers and all(w.done() for w in queue.workers):
                    break
                await asyncio.sleep(0.05)
                elapsed += 0.05
                if timeout is not None and elapsed >= timeout:
                    break
            await self._stop_workers()
            await self._compute_final_status()
            return self.status
        finally:
            _unregister_team_run(self.id)

    async def _stop_workers(self) -> None:
        self.cancel_event.set()
        if self.task_queue is not None:
            await self.task_queue.drain_and_stop()
        self.cancel_event.clear()

    async def _compute_final_status(self) -> None:
        if self._fatal_failure_reason:
            self.status = TeamRunStatus.FAILED
            return

        # Run status reflects the root task outcome. Any FAILED update aborts
        # via fail_fast before reaching this path, so non-fatal completions
        # leave the root at done / cancelled.
        root_status: str | None = None
        if self.root_task_id:
            rec = await self.task_center.store.get_record(self.root_task_id)
            if rec is not None:
                root_status = rec.status
        if root_status == "failed":
            self.status = TeamRunStatus.FAILED
        elif root_status == "cancelled":
            self.status = TeamRunStatus.CANCELLED
        else:
            self.status = TeamRunStatus.SUCCEEDED
        self.event_store.append(make_team_run_status(self.id, self.status.value))

    async def fail_fast(self, reason: str) -> None:
        """Stop execution and mark the whole team run failed immediately."""
        logger.critical(reason)
        if self._fatal_failure_reason is None:
            self._fatal_failure_reason = reason
            self.status = TeamRunStatus.FAILED
            self.event_store.append(make_team_run_status(self.id, self.status.value, reason=reason))
        self.cancel_event.set()
        for runner_task in list(self._active_agent_runs.values()):
            if not runner_task.done():
                runner_task.cancel()
        await self.task_center.store.cancel_all_pending()
        await self.task_center.store.cancel_all_running(reason)

    async def cancel(self) -> None:
        self.cancel_event.set()
        await self.task_center.store.cancel_all_pending()

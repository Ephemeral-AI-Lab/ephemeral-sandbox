"""TeamRun lifecycle container."""

from __future__ import annotations

import asyncio
import logging
import os
import uuid
from dataclasses import asdict
from typing import Any, Callable

from team.memory.runtime import persist_memory_record
from team.persistence.events import make_team_run_created, make_team_run_status
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.models import BudgetConfig, BudgetState, Task, TeamRunStatus
from team.runtime.executor import Executor
from team.runtime.rehydration import (
    apply_replayed_event,
    build_resumed_run,
    restore_ready_queue,
)
from team.runtime.registry import register as _register_team_run
from team.runtime.registry import unregister as _unregister_team_run
from team.runtime.services import TeamRuntimeServices, build_team_runtime_services


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
        self.task_center.set_cancel_running_task_callback(self.cancel_running_task)
        self.dispatch_queue = runtime_services.dispatch_queue
        self.budgets = self.task_center.budgets
        self.budget_state = self.task_center.budget_state
        self.project_context = runtime_services.project_context
        self.event_store: TeamRunStore = getattr(
            runtime_services, "event_store", NullTeamRunStore()
        )
        self.cancel_event = asyncio.Event()
        self.root_task_id: str | None = None
        self._executor_tasks: list[asyncio.Task[None]] = []
        self._executor_factory: Callable[["TeamRun"], Executor] | None = None
        self._num_executors: int = _default_num_executors()
        self._dispatching = 0
        self.coordination_metadata: dict[str, Any] = {}
        self.roster: dict[str, list[str]] = {}
        self.team_definition: Any | None = None
        self.arbiter: Any = getattr(runtime_services, "arbiter", None)

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

    # ---- lifecycle -------------------------------------------------------

    async def start(
        self,
        agent_name: str,
        payload: dict[str, Any],
        *,
        executor_factory: Callable[["TeamRun"], Executor],
        num_executors: int | None = None,
    ) -> None:
        from team.models import Task, TaskStatus

        objective = str(payload.get("objective") or payload.get("user_request") or "").strip()
        if not objective:
            raise ValueError("Root payload requires a non-empty 'objective'")
        root = Task(
            id=str(uuid.uuid4()),
            team_run_id=self.id,
            agent_name=agent_name,
            status=TaskStatus.PENDING,
            objective=objective,
            scope_paths=list(payload.get("scope_paths", [])),
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
        await self.task_center.add_task(root)
        self.status = TeamRunStatus.RUNNING
        self.event_store.append(make_team_run_status(self.id, self.status.value))
        _register_team_run(self)
        self._executor_factory = executor_factory
        if num_executors is not None:
            self._num_executors = max(1, int(num_executors))
        self._spawn_executors()

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

    def _spawn_executors(self) -> None:
        assert self._executor_factory is not None
        for _ in range(self._num_executors):
            executor = self._executor_factory(self)
            self._executor_tasks.append(asyncio.create_task(executor.run_forever()))

    async def _is_all_terminal(self) -> bool:
        return await self.task_center.store.all_terminal()

    async def wait(self, *, timeout: float | None = None) -> TeamRunStatus:
        try:
            elapsed = 0.0
            while not await self._is_all_terminal():
                if self._executor_tasks and all(t.done() for t in self._executor_tasks):
                    break
                await asyncio.sleep(0.05)
                elapsed += 0.05
                if timeout is not None and elapsed >= timeout:
                    break
            await self._join_executors()
            await self._compute_final_status()
            return self.status
        finally:
            _unregister_team_run(self.id)

    async def _join_executors(self) -> None:
        await self._stop_executors()

    async def _drain_executors(self) -> None:
        await self._stop_executors()
        await self.task_center.cancel_all_running("drained by rollback/cancel")

    async def _stop_executors(self) -> None:
        self.cancel_event.set()
        for t in self._executor_tasks:
            try:
                await asyncio.wait_for(t, timeout=2.0)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                t.cancel()
        self._executor_tasks = []
        self.cancel_event.clear()

    async def _compute_final_status(self) -> None:
        if self._fatal_failure_reason:
            self.status = TeamRunStatus.FAILED
            return

        # Safety net: force-fail any tasks still stuck in REQUEST_REPLAN before
        # computing final status — prevents silent success on orphaned replans.
        await self.task_center.fail_orphaned_replanning()

        # Run status reflects the root task outcome. Leaf failures are absorbed
        # by replan/detach semantics; they only propagate to the run when the
        # failure cascade reaches the root (all_children_detached).
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
        await self.task_center.cancel_all_pending()
        await self.task_center.cancel_all_running(reason)

    async def fail_after_active_work(self, reason: str) -> None:
        """Fail the run without cancelling agent turns already in flight.

        Use this for budget exhaustion discovered after an agent made a
        terminal submission. New work should stop, but active agent loops must
        still get a chance to reach their own terminal submission path.
        """
        logger.critical(reason)
        if self._fatal_failure_reason is None:
            self._fatal_failure_reason = reason
            self.status = TeamRunStatus.FAILED
            self.event_store.append(make_team_run_status(self.id, self.status.value, reason=reason))
        self.cancel_event.set()
        await self.task_center.cancel_all_pending()

    async def cancel(self) -> None:
        self.cancel_event.set()
        await self.task_center.cancel_all_pending()

    def note_conflict_event(
        self, *, file_path: str, reason: str, work_item_id: str = "", agent_name: str = ""
    ) -> bool:
        return persist_memory_record(
            project_key=self.project_context.project_key,
            repo_root=self.project_context.repo_root,
            kind="conflict_event",
            scope={"paths": [file_path] if file_path else []},
            content={"file_path": file_path, "reason": reason},
            source={"team_run_id": self.id, "work_item_id": work_item_id, "agent": agent_name},
            stale_hint="coordination conflict observed during live execution",
        )

    def note_validator_outcome(self, *, task: Task, summary: str) -> bool:
        return persist_memory_record(
            project_key=self.project_context.project_key,
            repo_root=self.project_context.repo_root,
            kind="validation_outcome",
            scope={"paths": list(task.scope_paths)},
            content={"task_id": task.id, "summary": summary},
            source={"team_run_id": self.id, "work_item_id": task.id, "agent": task.agent_name},
            stale_hint="validator result captured during live execution",
        )

    # ---- checkpoint API --------------------------------------------------

    async def checkpoint(self, label: str | None = None) -> str:
        cp = await self.task_center.checkpoint(label=label, project_context=self.project_context)
        return cp.id

    async def rollback_to(self, checkpoint_id: str) -> None:
        self.cancel_event.set()
        await self._drain_executors()
        await self.task_center.rollback_to(
            checkpoint_id,
            project_context_setter=lambda pc: setattr(self, "project_context", pc),
        )
        self.cancel_event.clear()
        if self._executor_factory is not None:
            self._spawn_executors()

    async def resume(
        self,
        *,
        executor_factory: Callable[["TeamRun"], Executor],
        num_executors: int | None = None,
        resumed_from: str | None = None,
        resumed_from_checkpoint: str | None = None,
    ) -> None:
        if await self._is_all_terminal():
            return
        await self.task_center.prepare_for_resume()
        self.cancel_event.clear()
        self._executor_factory = executor_factory
        if num_executors is not None:
            self._num_executors = max(1, int(num_executors))
        self.status = TeamRunStatus.RUNNING
        self.event_store.append(
            make_team_run_status(
                self.id,
                self.status.value,
                resumed_from=resumed_from,
                resumed_from_checkpoint=resumed_from_checkpoint,
            )
        )
        _register_team_run(self)
        self._spawn_executors()

    # ---- crash recovery --------------------------------------------------

    @classmethod
    def resume_from(
        cls, store: TeamRunStore, team_run_id: str, *, checkpoint_id: str | None = None
    ) -> "TeamRun":
        events = store.load_run(team_run_id)
        if not events:
            raise ValueError(f"no events for team_run_id={team_run_id!r}")
        if checkpoint_id is not None:
            cp_event = next(
                (
                    ev
                    for ev in events
                    if ev.kind == "checkpoint_taken"
                    and str(ev.data.get("checkpoint_id") or "") == checkpoint_id
                ),
                None,
            )
            if cp_event is None:
                raise ValueError(
                    f"checkpoint_id={checkpoint_id!r} not found for team_run_id={team_run_id!r}"
                )
            events = [ev for ev in events if ev.seq <= cp_event.seq]
        created = next((e for e in events if e.kind == "team_run_created"), None)
        if created is None:
            raise ValueError(f"event log for {team_run_id!r} missing team_run_created header")
        services, run = build_resumed_run(
            team_run_cls=cls, store=store, team_run_id=team_run_id, created_event=created
        )
        tc = services.task_center
        graph = tc.graph
        last_budget: tuple[int, int, int] | None = None
        final_status: str | None = None
        root_id: str | None = None
        for ev in events:
            root_id, replayed_budget, replayed_status = apply_replayed_event(
                event=ev,
                graph=graph,
                services=services,
                root_id=root_id,
            )
            if replayed_budget is not None:
                last_budget = replayed_budget
            if replayed_status is not None:
                final_status = replayed_status
        if last_budget is not None:
            run.budget_state.tasks_used = last_budget[0]
            run.budget_state.note_bytes_used = last_budget[1]
            run.budget_state.replans_used = last_budget[2]
        else:
            run.budget_state.tasks_used = len(graph)
        tc.prime_resume_state(
            snapshot=list(graph.values()),
            ready_queue_order=restore_ready_queue(graph=graph),
        )
        run.root_task_id = root_id
        if final_status:
            try:
                run.status = TeamRunStatus(final_status)
            except ValueError:
                pass
        return run

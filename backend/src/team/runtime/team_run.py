"""TeamRun lifecycle container."""

from __future__ import annotations

import asyncio
import os
import uuid
from dataclasses import asdict
from typing import Any, Callable

from team.memory.runtime import persist_memory_record
from team.persistence.events import (
    make_team_run_created,
    make_team_run_status,
)
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.models import (
    BudgetConfig,
    BudgetState,
    Task,
    TeamDefinition,
    TeamRunStatus,
)
from team.task_center import TaskCenter
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
        runtime_services = services or build_team_runtime_services(
            team_run_id=self.id,
            budgets=self.budgets,
            budget_state=self.budget_state,
            user_request=user_request,
            goal=goal,
            repo_root=repo_root,
            event_store=event_store,
        )
        self.budgets = runtime_services.dispatcher.budgets
        self.budget_state = runtime_services.dispatcher.budget_state
        self.project_context = runtime_services.project_context
        self.dispatcher = runtime_services.dispatcher
        self.event_store: TeamRunStore = getattr(
            runtime_services, "event_store", NullTeamRunStore()
        )
        self.cancel_event = asyncio.Event()
        self.root_work_item_id: str | None = None
        self._executor_tasks: list[asyncio.Task[None]] = []
        self._executor_factory: Callable[["TeamRun"], Executor] | None = None
        self._num_executors: int = _default_num_executors()
        # Per-run metadata injected into every work-item's ExecutionMetadata.
        # Benchmark runners set team_mode_enabled, enforcement flags, etc.
        self.coordination_metadata: dict[str, Any] = {}
        # Role → agent-name mapping from the TeamDefinition.  Stored at
        # start time so context builders can render it into planner prompts.
        self.roster: dict[str, list[str]] = {}
        # Shared context log for all tasks in this run.
        self.task_center = TaskCenter(goal=goal or "", user_request=user_request)
        # Wire TaskCenter into dispatcher for cascade "continue" note injection
        self.dispatcher.task_center = self.task_center
        # Optional Ledger reference for file-change awareness in context_for().
        # Set by the caller if CodeIntelligenceService is available.
        self.ledger: Any = None
        # FileChangeStore for cross-run edit history. Defaults to NullFileChangeStore
        # (no-op) when PostgreSQL is unavailable. Caller replaces with real store.
        from team.persistence.file_change_store import NullFileChangeStore
        self.file_change_store: Any = NullFileChangeStore()

    # ---- lifecycle -------------------------------------------------------

    async def start(
        self,
        agent_name: str,
        payload: dict[str, Any],
        *,
        executor_factory: Callable[["TeamRun"], Executor],
        num_executors: int | None = None,
        root_kind: str = "atomic",
    ) -> None:
        from team.models import Task, TaskStatus
        root = Task(
            id=str(uuid.uuid4()),
            team_run_id=self.id,
            agent_name=agent_name,
            status=TaskStatus.PENDING,
            task=payload.get("task", payload.get("user_request", str(payload))),
            scope_paths=list(payload.get("scope_paths", [])),
            depth=0,
        )
        root.root_id = root.id
        self.root_work_item_id = root.id
        # Durable record of the run *before* any work items exist so a
        # crash during dispatch still leaves a recoverable header.
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
        await self.dispatcher.add_work_item(root)
        self.status = TeamRunStatus.RUNNING
        self.event_store.append(make_team_run_status(self.id, self.status.value))
        _register_team_run(self)
        self._executor_factory = executor_factory
        if num_executors is not None:
            self._num_executors = max(1, int(num_executors))
        self._spawn_executors()

    async def start_with_team_definition(
        self,
        team_def: TeamDefinition,
        payload: dict[str, Any],
        *,
        executor_factory: Callable[["TeamRun"], Executor],
        num_executors: int | None = None,
    ) -> None:
        """Start a team run using the team definition's ``entry_planner``.

        Validates that ``entry_planner`` resolves in ``agents.registry``
        before dispatching the root Task.
        """
        from agents.registry import get_definition

        if get_definition(team_def.entry_planner) is None:
            raise ValueError(
                f"team_definition '{team_def.name}' entry_planner "
                f"'{team_def.entry_planner}' does not exist"
            )
        self.roster = dict(team_def.roster)
        await self.start(
            agent_name=team_def.entry_planner,
            payload=payload,
            executor_factory=executor_factory,
            num_executors=num_executors,
            root_kind="expandable",
        )

    def _spawn_executors(self) -> None:
        assert self._executor_factory is not None, "executor_factory not set"
        for _ in range(self._num_executors):
            executor = self._executor_factory(self)
            self._executor_tasks.append(asyncio.create_task(executor.run_forever()))

    async def wait(self, *, timeout: float | None = None) -> TeamRunStatus:
        try:
            elapsed = 0.0
            while not self.dispatcher.all_terminal():
                if self._executor_tasks and all(t.done() for t in self._executor_tasks):
                    # All executors died but DAG is not terminal — break to avoid infinite loop
                    break
                await asyncio.sleep(0.05)
                elapsed += 0.05
                if timeout is not None and elapsed >= timeout:
                    break
            await self._join_executors()
            self._compute_final_status()
            return self.status
        finally:
            _unregister_team_run(self.id)

    async def _join_executors(self) -> None:
        """Cooperative shutdown after the DAG has reached a terminal state.

        Unlike ``_drain_executors``, this does NOT cancel running items —
        the graph is already terminal so no item should be RUNNING.
        """
        await self._stop_executors()

    async def _drain_executors(self) -> None:
        """Forceful drain used by rollback/cancel — kills any RUNNING item."""
        await self._stop_executors()
        await self.dispatcher.cancel_running("drained by rollback/cancel")

    async def _stop_executors(self) -> None:
        self.cancel_event.set()
        for t in self._executor_tasks:
            try:
                await asyncio.wait_for(t, timeout=2.0)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                t.cancel()
        self._executor_tasks = []
        self.cancel_event.clear()

    def _compute_final_status(self) -> None:
        statuses = {str(wi.status.value) for wi in self.dispatcher.graph.values()}
        if "failed" in statuses:
            self.status = TeamRunStatus.FAILED
        elif "cancelled" in statuses:
            self.status = TeamRunStatus.CANCELLED
        else:
            self.status = TeamRunStatus.SUCCEEDED
        self.event_store.append(make_team_run_status(self.id, self.status.value))

    async def cancel(self) -> None:
        self.cancel_event.set()
        await self.dispatcher.cancel_all_pending()

    def note_validator_outcome(
        self,
        *,
        task: "Task",
        summary: str,
    ) -> bool:
        scope_paths = list(task.scope_paths)
        return persist_memory_record(
            project_key=self.project_context.project_key,
            repo_root=self.project_context.repo_root,
            kind="validation_outcome",
            scope={"paths": scope_paths},
            content={
                "summary": summary,
                "task_id": task.id,
                "agent_name": task.agent_name,
            },
            source={
                "team_run_id": self.id,
                "task_id": task.id,
                "agent": task.agent_name,
            },
        )

    def note_conflict_event(
        self,
        *,
        file_path: str,
        reason: str,
        work_item_id: str = "",
        agent_name: str = "",
    ) -> bool:
        return persist_memory_record(
            project_key=self.project_context.project_key,
            repo_root=self.project_context.repo_root,
            kind="conflict_event",
            scope={"paths": [file_path] if file_path else []},
            content={
                "file_path": file_path,
                "reason": reason,
            },
            source={
                "team_run_id": self.id,
                "work_item_id": work_item_id,
                "agent": agent_name,
            },
            stale_hint="coordination conflict observed during live execution",
        )

    # ---- checkpoint API --------------------------------------------------

    async def checkpoint(self, label: str | None = None) -> str:
        cp = await self.dispatcher.checkpoint(
            label=label,
            project_context=self.project_context,
        )
        return cp.id

    async def rollback_to(self, checkpoint_id: str) -> None:
        # Phase 1 — cooperative drain.
        self.cancel_event.set()
        await self._drain_executors()
        # Phase 2 — atomic restore.
        await self.dispatcher.rollback_to(
            checkpoint_id,
            project_context_setter=lambda pc: setattr(self, "project_context", pc),
        )
        self.cancel_event.clear()
        # Phase 3 — respawn workers so the restored DAG actually drains.
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
        """Resume a rehydrated TeamRun in the current process."""
        if self.dispatcher.all_terminal():
            return

        await self.dispatcher.prepare_for_resume()
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
        cls,
        store: TeamRunStore,
        team_run_id: str,
        *,
        checkpoint_id: str | None = None,
    ) -> "TeamRun":
        """Rehydrate a TeamRun from its durable event log.

        Replays every event emitted for ``team_run_id`` back into a
        fresh set of runtime objects:

        * ``Dispatcher.graph`` is reconstructed from ``work_item_added``
          events plus the final ``work_item_status`` seen for each id.
        * ``BudgetState`` is set from the last ``budget_update`` event
          (fallback: counted from graph size).
        * The ready queue is rebuilt to hold every Task that ended
          up in ``READY`` status at the end of the log.

        The returned TeamRun is **paused**: no executors are running.
        Callers resume it explicitly via ``TeamRun.resume(...)`` so they
        can decide whether to finish the run, inspect it, or cancel.

        Raises ``ValueError`` if no events exist for ``team_run_id`` or
        the log lacks a ``team_run_created`` header.
        """
        events = store.load_run(team_run_id)
        if not events:
            raise ValueError(f"no events for team_run_id={team_run_id!r}")

        if checkpoint_id is not None:
            checkpoint_event = next(
                (
                    ev
                    for ev in events
                    if ev.kind == "checkpoint_taken"
                    and str(ev.data.get("checkpoint_id") or "") == checkpoint_id
                ),
                None,
            )
            if checkpoint_event is None:
                raise ValueError(
                    f"checkpoint_id={checkpoint_id!r} not found for team_run_id={team_run_id!r}"
                )
            events = [ev for ev in events if ev.seq <= checkpoint_event.seq]

        created = next((e for e in events if e.kind == "team_run_created"), None)
        if created is None:
            raise ValueError(
                f"event log for {team_run_id!r} missing team_run_created header"
            )

        services, run = build_resumed_run(
            team_run_cls=cls,
            store=store,
            team_run_id=team_run_id,
            created_event=created,
        )

        # --- fold events into runtime state -----------------------------
        graph = services.dispatcher.graph
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

        services.dispatcher._ready_order = restore_ready_queue(
            dispatcher=services.dispatcher,
            graph=graph,
        )

        run.root_work_item_id = root_id
        if final_status:
            try:
                run.status = TeamRunStatus(final_status)
            except ValueError:
                pass

        return run

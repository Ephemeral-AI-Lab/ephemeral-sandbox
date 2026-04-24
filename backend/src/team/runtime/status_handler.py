"""TaskStatusHandler — single dispatch entry for every task status change.

One match block, six cases:

- ``SUCCESS``                          — mark done + cascade promotions
- ``EXPANDED``                         — insert plan/replan children
- ``EXPANDED_AWAITING_SUMMARY``        — inject parent-summary sidecar
- ``REQUEST_REPLAN``                   — spawn recovery replanner
- ``CANCELLED``                        — cascade cancel
- ``FAILED``                           — mark failed + fail-fast the run

``handle()`` wraps the match block in ``async with self._lock`` so N workers
calling it concurrently see a single-transition site. Re-entry from inside a
case calls ``self._dispatch`` directly (the lock is already held).

The only writer outside this handler is ``TaskExecutor.mark_running`` — the
atomic ``ready → running`` claim — which is documented, not enforced.
"""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Iterable
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from team.errors import BudgetExceeded, GraphInvariantViolation, InvalidPlan
from team.models import (
    TERMINAL_STATUSES,
    LeafSubmission,
    Plan,
    PlannerSubmission,
    ReplanPlan,
    SubmittedSummary,
    Task,
    TaskStatus,
    TaskStatusUpdate,
)
from team.task_center.budget import BudgetManager
from team.persistence.events import (
    TeamRunEvent,
    make_replace_dependency,
    make_task_added,
    make_task_status,
    task_to_dict,
)
from team.persistence.task_store import (
    TaskStore,
    _has_parent_summarizer_role,
    _has_replanner_role,
)
from team.planning.expander import PlanExpander

if TYPE_CHECKING:
    from team.runtime.task_queue import TaskQueue

logger = logging.getLogger(__name__)


RosterGetter = Callable[[], dict[str, list[str]] | None]


class TaskStatusHandler:
    """Lock-serialized sink for every ``TaskStatusUpdate``."""

    def __init__(
        self,
        *,
        team_run_id: str,
        store: TaskStore,
        budget: BudgetManager,
        expander: PlanExpander,
        emit_event: Callable[[TeamRunEvent], None],
        fail_fast: Callable[[str], Awaitable[None]],
        cancel_running_task: Callable[[str], None] | None = None,
        cancel_event: asyncio.Event | None = None,
        roster_getter: RosterGetter | None = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._store = store
        self._budget = budget
        self._expander = expander
        self._emit = emit_event
        self._fail_fast = fail_fast
        self._cancel_running_task = cancel_running_task
        self._cancel_event = cancel_event
        self._roster_getter = roster_getter
        self._queue: "TaskQueue | None" = None
        self._lock = asyncio.Lock()

    # ---- wiring ----------------------------------------------------------

    def bind_queue(self, queue: "TaskQueue") -> None:
        self._queue = queue

    def bind_cancel_event(self, event: asyncio.Event) -> None:
        self._cancel_event = event

    # ---- public entry points --------------------------------------------

    async def handle(self, update: TaskStatusUpdate) -> None:
        """Lock-serialized single dispatch entry."""
        async with self._lock:
            before = self._snapshot_transitions()
            await self._dispatch(update)
            await self._refresh_and_emit(before)

    async def on_task_added(self, task: Task) -> None:
        """Hook called after the root task (or an externally-inserted task).

        Reads the live graph state so the caller can pass the original
        in-memory ``Task`` (pre-``add_task``) without having to reread it.
        """
        async with self._lock:
            current = self._store.get_task(task.id) or task
            if current.status == TaskStatus.READY:
                self._enqueue(current.id)

    async def recover_awaiting_summary_parents(self) -> None:
        """Re-inject parent-summary sidecars on restart.

        Any parent stuck in ``expanded_awaiting_summary`` with no live
        summarizer gets one spawned.
        """
        fetcher = getattr(self._store, "fetch_parents_awaiting_summary", None)
        if fetcher is None:
            return
        async with self._lock:
            before = self._snapshot_transitions()
            stuck = await fetcher()
            for parent_id in stuck:
                try:
                    sidecar_id = await self._inject_parent_summary(parent_id)
                    if sidecar_id is not None:
                        self._enqueue(sidecar_id)
                except Exception:
                    logger.exception(
                        "failed to re-inject parent-summary sidecar for %s",
                        parent_id,
                    )
            await self._refresh_and_emit(before)

    # ---- dispatch (single match site) -----------------------------------

    async def _dispatch(self, update: TaskStatusUpdate) -> None:
        status = update.status
        if status is TaskStatus.DONE:
            await self._on_success(update)
        elif status is TaskStatus.EXPANDED:
            await self._on_expanded(update)
        elif status is TaskStatus.EXPANDED_AWAITING_SUMMARY:
            await self._on_awaiting_summary(update)
        elif status is TaskStatus.REQUEST_REPLAN:
            await self._on_request_replan(update)
        elif status is TaskStatus.CANCELLED:
            await self._on_cancelled(update)
        elif status is TaskStatus.FAILED:
            await self._on_failed(update)
        else:
            raise ValueError(f"Unsupported TaskStatusUpdate.status: {status!r}")

    # ---- SUCCESS --------------------------------------------------------

    async def _on_success(self, update: TaskStatusUpdate) -> None:
        task_id = update.task_id
        summary = update.summary or ""
        promoted_ready = await self._store.mark_done(task_id)
        self._enqueue_many(promoted_ready)
        task = self._store.get_task(task_id)

        if task is not None and task.submission is None:
            task.submission = LeafSubmission(summary=SubmittedSummary(summary=summary))

        if task is not None and _is_parent_summary_sidecar(task):
            parent_id = task.fired_by_task_id
            if not parent_id:
                await self._dispatch(_fail(task_id, "parent_summary_missing_parent"))
                return
            if summary.strip():
                parent = self._store.get_task(parent_id)
                if parent is not None:
                    if isinstance(parent.submission, PlannerSubmission):
                        parent.submission.summary = SubmittedSummary(summary=summary.strip())
                    else:
                        parent.submission = PlannerSubmission(
                            plan=Plan(),
                            summary=SubmittedSummary(summary=summary.strip()),
                        )
                await self._finalize_awaiting_summary(parent_id)
            else:
                await self._dispatch(
                    TaskStatusUpdate(
                        task_id=parent_id,
                        status=TaskStatus.FAILED,
                        summary="parent_summary_empty",
                    )
                )
                return

        # If the completed task is a replanner, resolve the origin chain.
        if self._is_replanner(task_id):
            await self._finalize_replanned_origin_chain(task_id)

        # Cascade promotion up expanded parent(s).
        await self._cascade_expanded_parent(task_id)

    async def _finalize_awaiting_summary(self, parent_id: str) -> None:
        """Transition an EAS parent → DONE and cascade further promotions."""
        rec = await self._store.get_record(parent_id)
        if rec is None or rec.status != TaskStatus.EXPANDED_AWAITING_SUMMARY.value:
            return
        promoted_ready = await self._store.finalize_parent_summary(parent_id)
        self._enqueue_many(promoted_ready)
        await self._cascade_expanded_parent(parent_id)
        if self._is_replanner(parent_id):
            await self._finalize_replanned_origin_chain(parent_id)

    async def _cascade_expanded_parent(self, child_id: str) -> None:
        """Walk up EXPANDED parents, promoting / emitting EAS injections."""
        promoted, awaiting = await self._store.maybe_promote_expanded_parent(child_id)
        await self._apply_promotions(promoted, awaiting)

    async def _apply_promotions(
        self, promoted: list[str], awaiting: list[str]
    ) -> None:
        """Enqueue newly READY ids, chain replanner origins, dispatch EAS parents."""
        self._enqueue_many(promoted)
        for promoted_id in promoted:
            if self._is_replanner(promoted_id):
                await self._finalize_replanned_origin_chain(promoted_id)
        for parent_id in awaiting:
            await self._dispatch(
                TaskStatusUpdate(
                    task_id=parent_id,
                    status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
                )
            )

    async def _finalize_replanned_origin_chain(self, replanner_id: str) -> None:
        """Recursively finalize REQUEST_REPLAN origins up the replanner chain."""
        origin_id = await self._store.finalize_replanned_origin(replanner_id)
        if origin_id is None:
            return
        await self._cascade_expanded_parent(origin_id)

    def _is_replanner(self, task_id: str) -> bool:
        task = self._store.get_task(task_id)
        return (
            task is not None
            and bool(task.fired_by_task_id)
            and _has_replanner_role(task.definition.agent)
        )

    # ---- EXPANDED -------------------------------------------------------

    async def _on_expanded(self, update: TaskStatusUpdate) -> None:
        task_id = update.task_id
        rec = await self._store.get_record(task_id)
        if rec is None:
            await self._dispatch(_fail(task_id, "expand_target_missing"))
            return
        try:
            if update.replan is not None:
                await self._expand_replan(rec, update.replan)
                return
            await self._expand_plan(rec, update.plan)
        except InvalidPlan as exc:
            await self._dispatch(_fail(task_id, f"InvalidPlan: {exc}"))
        except BudgetExceeded as exc:
            await self._dispatch(_fail(task_id, f"BudgetExceeded: {exc}"))
        except GraphInvariantViolation as exc:
            await self._dispatch(_fail(task_id, f"GraphInvariantViolation: {exc}"))

    async def _expand_plan(self, rec: Any, plan: Plan | None) -> None:
        outcome = await self._expander.expand_submitted_plan(rec, plan)
        if plan is None:
            # No children to wait on — finalize directly.
            promoted_ready = await self._store.mark_done(rec.id)
            self._enqueue_many(promoted_ready)
            await self._cascade_expanded_parent(rec.id)
            return
        await self._store.mark_expanded(rec.id)
        planner_task = self._store.get_task(rec.id)
        if planner_task is not None:
            planner_task.submission = PlannerSubmission(plan=plan)
        self._enqueue_many(item.id for item in outcome.new_items)

    async def _expand_replan(self, rec: Any, replan: ReplanPlan) -> None:
        outcome = await self._expander.apply_replan(
            replan_task_id=rec.id,
            add_tasks=list(replan.add_tasks),
            cancel_ids=list(replan.cancel_ids),
        )
        replanner_task = self._store.get_task(rec.id)
        if replanner_task is not None:
            replanner_task.submission = PlannerSubmission(plan=replan)
        if self._cancel_running_task is not None:
            for running_id in outcome.cancelled_running_ids:
                self._cancel_running_task(running_id)
        if outcome.replanner_child_count > 0:
            await self._store.mark_expanded(rec.id)
            await self._finalize_replanned_origin_chain(rec.id)
            self._enqueue_many(outcome.inserted_ids)
        else:
            # Empty replan (no corrective tasks): the replanner did not do
            # its job, so fail it rather than synthesizing a success summary.
            await self._dispatch(_fail(rec.id, "replan_produced_no_corrective_tasks"))
        # Replan cancels may have detached whole subtrees; sweep parents.
        promoted, awaiting = await self._store.sweep_expanded_promotions()
        await self._apply_promotions(promoted, awaiting)

    # ---- EXPANDED_AWAITING_SUMMARY -------------------------------------

    async def _on_awaiting_summary(self, update: TaskStatusUpdate) -> None:
        """Inject a parent-summary sidecar for an EAS parent.

        The DB row has already been set to ``expanded_awaiting_summary`` by
        ``TaskStore.maybe_promote_expanded_parent``; this case only injects
        the summarizer task.
        """
        sidecar_id = await self._inject_parent_summary(update.task_id)
        if sidecar_id is not None:
            self._enqueue(sidecar_id)

    # ---- REQUEST_REPLAN ------------------------------------------------

    async def _on_request_replan(self, update: TaskStatusUpdate) -> None:
        task_id = update.task_id
        try:
            self._budget.require_replan_capacity()
        except BudgetExceeded as exc:
            await self._dispatch(_fail(task_id, f"replan_budget_exhausted: {exc}"))
            return

        replanner_agent = _first_replanner_name()
        if replanner_agent is None:
            await self._dispatch(_fail(task_id, "no_replanner_registered"))
            return

        deps_before = {tid: list(task.definition.deps) for tid, task in self._store.graph.items()}
        rec, is_new = await self._store.request_replan(
            task_id,
            reason=update.summary or "",
            suggestion=None,
            replanner_agent=replanner_agent,
        )
        if is_new:
            self._budget.bump_replan_counters()
            replanner_task = self._store.graph.get(rec.id)
            if replanner_task is not None:
                self._emit(make_task_added(self._team_run_id, task_to_dict(replanner_task)))
            rewired = [
                tid
                for tid, old_deps in deps_before.items()
                if task_id in old_deps
                and rec.id in getattr(self._store.graph.get(tid), "deps", [])
            ]
            self._emit(
                make_replace_dependency(
                    self._team_run_id,
                    old_dep_id=task_id,
                    new_dep_ids=[rec.id],
                    task_ids=rewired,
                )
            )
            self._budget.emit_update()
        self._enqueue(rec.id)

    # ---- CANCELLED -----------------------------------------------------

    async def _on_cancelled(self, update: TaskStatusUpdate) -> None:
        reason = update.summary or "cancelled"
        await self._store.mark_terminal(update.task_id, "cancelled", reason)
        await self._store.cascade_cancel_recursive(update.task_id)

    # ---- FAILED --------------------------------------------------------

    async def _on_failed(self, update: TaskStatusUpdate) -> None:
        reason = update.summary or "failed"
        await self._store.mark_failed(update.task_id, reason)
        # Idempotent: if fail-fast is already in flight, observe the FAILED
        # row but skip re-triggering the run-level cancel wave.
        if self._cancel_event is not None and self._cancel_event.is_set():
            return
        await self._fail_fast(reason)

    # ---- parent-summary sidecar injection ------------------------------

    async def _inject_parent_summary(self, parent_id: str) -> str | None:
        parent = self._store.graph.get(parent_id)
        if parent is None:
            await self._store.refresh_graph()
            parent = self._store.graph.get(parent_id)
            if parent is None:
                logger.warning(
                    "inject_parent_summary: parent %s missing from graph", parent_id
                )
                return None

        if _has_recorded_parent_summary(parent):
            await self._finalize_awaiting_summary(parent_id)
            return None

        live_sidecar = next(
            (
                task
                for task in self._store.graph.values()
                if task.fired_by_task_id == parent_id
                and task.status not in TERMINAL_STATUSES
                and _has_parent_summarizer_role(task.definition.agent)
            ),
            None,
        )
        if live_sidecar is not None:
            return live_sidecar.id

        from prompt.external_trigger_prompts import build_parent_summary_prompt

        children = [
            t for t in self._store.graph.values()
            if getattr(t, "parent_id", None) == parent_id
        ]
        summary_task, created = await self._store.insert_parent_summary_task(
            parent_task=parent,
            summarizer_agent=self._resolve_parent_summarizer_agent(),
            objective=build_parent_summary_prompt(parent, children),
        )
        if created:
            self._emit(make_task_added(self._team_run_id, task_to_dict(summary_task)))
        return summary_task.id

    def _resolve_parent_summarizer_agent(self) -> str:
        """Pick the roster's parent_summarizer; fall back to canonical name."""
        default = "parent_summarizer"
        roster = self._roster_getter() if self._roster_getter is not None else None
        if not isinstance(roster, dict):
            return default
        candidates = roster.get("parent_summarizer")
        if not isinstance(candidates, list):
            return default
        try:
            from agents.registry import get_definition
        except Exception:
            return default
        for candidate in candidates:
            name = str(candidate).strip()
            if not name:
                continue
            defn = get_definition(name)
            if defn is None or getattr(defn, "role", None) != "parent_summarizer":
                continue
            return name
        return default

    # ---- queue / emit helpers ------------------------------------------

    def _enqueue(self, task_id: str) -> None:
        """Push ``task_id`` onto the ready queue iff its status is READY.

        Defense-in-depth: the handler walks graph-level returns that mix
        newly-ready dependents with just-finalized parents; we only want to
        enqueue the former.
        """
        if self._queue is None:
            return
        task = self._store.get_task(task_id)
        if task is None or task.status != TaskStatus.READY:
            return
        self._queue.enqueue(task_id)

    def _enqueue_many(self, task_ids: Iterable[str]) -> None:
        for tid in task_ids:
            self._enqueue(tid)

    # ---- transition emission ------------------------------------------

    def _snapshot_transitions(self) -> dict[str, tuple[Any, ...] | None]:
        return {
            tid: _task_signature(task)
            for tid, task in self._store.graph.items()
        }

    async def _refresh_and_emit(
        self, before: dict[str, tuple[Any, ...] | None]
    ) -> None:
        await self._store.refresh_graph()
        for task_id, prior in before.items():
            task = self._store.graph.get(task_id)
            if task is None:
                continue
            current = _task_signature(task)
            if current == prior:
                continue
            self._emit(
                make_task_status(
                    self._team_run_id,
                    task.id,
                    task.status.value,
                    agent_run_id=task.agent_run_id,
                    started_at=_iso(task.started_at),
                    finished_at=_iso(task.finished_at),
                    failure_reason=task.failure_reason,
                )
            )


# ---- module-level helpers ----------------------------------------------


def _fail(task_id: str, reason: str) -> TaskStatusUpdate:
    return TaskStatusUpdate(task_id=task_id, status=TaskStatus.FAILED, summary=reason)


def _is_parent_summary_sidecar(task: Task) -> bool:
    return bool(task.fired_by_task_id) and _has_parent_summarizer_role(
        task.definition.agent
    )


def _has_recorded_parent_summary(task: Task) -> bool:
    submission = task.submission
    if not isinstance(submission, PlannerSubmission):
        return False
    summary = submission.summary
    return bool(summary is not None and summary.summary.strip())


def _first_replanner_name() -> str | None:
    from agents.registry import find_by_role

    replanners = find_by_role("replanner")
    return replanners[0].name if replanners else None


def _iso(value: Any) -> str | None:
    return value.isoformat() if value is not None else None


def _task_signature(task: Task | None) -> tuple[Any, ...] | None:
    if task is None:
        return None
    return (
        task.status.value,
        task.agent_run_id,
        _iso(task.started_at),
        _iso(task.finished_at),
        task.failure_reason,
    )

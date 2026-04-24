"""TaskCoordinator — single owner of every task status change.

Outcome-driven transitions go through ``handle()`` (one match block, five
cases) under an asyncio lock so concurrent workers never interleave graph
mutations:

- ``DONE``              — mark done + cascade promotions
- ``EXPANDED``          — insert plan/replan children
- ``REQUEST_REPLAN``    — spawn recovery replanner
- ``CANCELLED``         — cascade cancel
- ``FAILED``            — mark failed + fail-fast the run

The atomic ``ready → running`` claim is exposed as ``claim_running()``; it is
the executor's intent-to-start handshake and also emits the ``running`` event.
Re-entry from inside a ``_dispatch`` case calls ``self._dispatch`` directly
(the lock is already held).
"""

from __future__ import annotations

import asyncio
from collections.abc import Iterable
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from team.core.errors import BudgetExceeded, GraphInvariantViolation, InvalidPlan
from team.core.models import (
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
from team.persistence.task_store import TaskStore, _has_replanner_role
from team.planning.expander import PlanExpander

if TYPE_CHECKING:
    from team.runtime.task_queue import TaskQueue


class TaskCoordinator:
    """Single owner for every task status transition."""

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
    ) -> None:
        self._team_run_id = team_run_id
        self._store = store
        self._budget = budget
        self._expander = expander
        self._emit = emit_event
        self._fail_fast = fail_fast
        self._cancel_running_task = cancel_running_task
        self._cancel_event = cancel_event
        self._queue: "TaskQueue | None" = None
        self._lock = asyncio.Lock()

    # ---- wiring ----------------------------------------------------------

    def bind_queue(self, queue: "TaskQueue") -> None:
        self._queue = queue

    # ---- public entry points --------------------------------------------

    async def claim_running(self, task_id: str, agent_run_id: str) -> Task | None:
        """Atomic ``ready → running`` claim for the executor.

        Returns the claimed ``Task`` on success, or ``None`` when the task is
        no longer in ``ready``/``running`` state (already claimed, cancelled,
        or missing). Emits the ``running`` status event as a side-effect.

        Runs lockless: ``store.mark_running`` is DB-atomic, and serializing
        every worker claim on the coordinator lock would bottleneck startup.
        """
        rec = await self._store.mark_running(task_id, agent_run_id)
        if rec is None:
            return None
        task = self._store.get_task(task_id)
        if task is None:
            return None
        self._emit(
            make_task_status(
                self._team_run_id,
                task_id,
                "running",
                agent_run_id=agent_run_id,
                started_at=_iso(task.started_at),
            )
        )
        return task

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

    # ---- dispatch (single match site) -----------------------------------

    async def _dispatch(self, update: TaskStatusUpdate) -> None:
        status = update.status
        if status is TaskStatus.DONE:
            await self._on_success(update)
        elif status is TaskStatus.EXPANDED:
            await self._on_expanded(update)
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

        # If the completed task is a replanner, resolve the origin chain.
        if self._is_replanner(task_id):
            await self._finalize_replanned_origin_chain(task_id)

        # Cascade promotion up expanded parent(s).
        await self._cascade_expanded_parent(task_id)

    async def _cascade_expanded_parent(self, child_id: str) -> None:
        """Walk up EXPANDED parents, synthesizing each parent's summary and marking DONE."""
        current = child_id
        while True:
            parent_id = await self._store.fetch_promotable_parent(current)
            if parent_id is None:
                return
            await self._finalize_expanded_parent(parent_id)
            current = parent_id

    async def _finalize_expanded_parent(self, parent_id: str) -> None:
        """Synthesize the planner's summary from children, mark DONE, chain promotions."""
        parent = self._store.get_task(parent_id)
        if parent is not None:
            summary = _synthesize_parent_summary(parent_id, self._store.graph)
            if isinstance(parent.submission, PlannerSubmission):
                parent.submission.summary = SubmittedSummary(summary=summary)
            else:
                parent.submission = PlannerSubmission(
                    plan=Plan(), summary=SubmittedSummary(summary=summary)
                )
        promoted_ready = await self._store.mark_done(parent_id)
        self._enqueue_many(promoted_ready)
        if self._is_replanner(parent_id):
            await self._finalize_replanned_origin_chain(parent_id)

    async def _sweep_promotable_parents(self) -> None:
        """Re-run promotion checks for parents whose children have all detached.

        Called after bulk graph changes (replan cancels, cascade) that can
        cause an EXPANDED parent to become promotable without a direct
        child-DONE event. Walks upward from every terminal child.
        """
        for child_id in self._store.terminal_child_ids():
            await self._cascade_expanded_parent(child_id)

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
        await self._sweep_promotable_parents()

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


def _synthesize_parent_summary(parent_id: str, graph: dict[str, Task]) -> str:
    """Build a parent's summary from its children.

    Priority:
    1. Terminal validator (reviewer role not depended on by any sibling,
       chosen by earliest ``created_at``) — use its submission summary.
    2. If no validator or the validator produced empty text, concatenate
       the summaries of terminal non-validator leaves ordered by
       ``created_at``, joined with ``\\n\\n---\\n\\n``.
    """
    children = [t for t in graph.values() if t.parent_id == parent_id]
    if not children:
        return ""
    from agents.registry import has_role

    sibling_deps = {d for c in children for d in (c.definition.deps or [])}
    terminal_validators = sorted(
        (c for c in children if has_role(c.definition.agent, "reviewer") and c.id not in sibling_deps),
        key=lambda t: t.created_at,
    )
    if terminal_validators:
        text = _extract_summary_text(terminal_validators[0])
        if text:
            return text
    leaves = [
        c for c in children
        if c.id not in sibling_deps and not has_role(c.definition.agent, "reviewer")
    ]
    parts = [
        text
        for text in (_extract_summary_text(c) for c in sorted(leaves, key=lambda t: t.created_at))
        if text
    ]
    return "\n\n---\n\n".join(parts)


def _extract_summary_text(task: Task) -> str:
    submission = task.submission
    if isinstance(submission, LeafSubmission):
        return submission.summary.summary.strip()
    if isinstance(submission, PlannerSubmission) and submission.summary is not None:
        return submission.summary.summary.strip()
    return ""


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

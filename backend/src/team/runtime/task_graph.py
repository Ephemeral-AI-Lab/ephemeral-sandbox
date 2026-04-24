"""TaskGraph — single in-memory owner of task graph mutation rules.

All policy lives here: dependent promotion, cascade cancellation, replan
spawning + dependency rewiring, replan application. Methods are **pure**:
they return a ``GraphMutation`` describing the change without mutating state.
Call ``graph.apply(mutation)`` to commit it to the in-memory Task dict and
``store.persist(mutation)`` to write it through to the database.

Mapping from displaced helpers:

- ``TaskStore.mark_done``                         → ``promote_on_done``
- ``TaskStore.mark_expanded``                     → ``mark_expanded``
- ``TaskStore.mark_terminal`` (cancelled path)    → ``cancel``
- ``TaskStore.mark_failed``                       → ``fail``
- ``TaskStore.fetch_promotable_parent``           → ``find_promotable_parent``
- ``TaskStore.cascade_cancel_recursive``          → ``compute_cancel_cascade`` + ``cancel_cascade``
- ``TaskStore.request_replan``                    → ``plan_request_replan``
- ``TaskStore.apply_replan_atomic`` (+ ``PlanExpander.apply_replan``)
                                                  → ``apply_replan``
- ``TaskStore.finalize_replanned_origin``         → ``finalize_replanned_origin``
- ``tasks_sql.replace_dependency`` invariant      → embedded in ``plan_request_replan``
- ``replan_validation._cascade_ids_for_cancel_root`` → ``compute_cancel_cascade``

The single place all mutation rules live means pre-submission validation
(``replan_validation``) and at-apply enforcement can share the same traversal
and invariant checks.
"""

from __future__ import annotations

import uuid
from collections import defaultdict, deque
from dataclasses import dataclass
from typing import Callable, Iterable

from team.core.errors import GraphInvariantViolation
from team.core.models import (
    TERMINAL_STATUSES,
    Task,
    TaskDefinition,
    TaskSpec,
    TaskStatus,
    _utcnow,
)

_FULLY_TERMINAL = frozenset(
    {TaskStatus.DONE, TaskStatus.FAILED, TaskStatus.CANCELLED}
)
"""Statuses we treat as immovable. REQUEST_REPLAN is detached but still
transition-able to FAILED (e.g., replanner-budget-exhausted path)."""


def _default_id_factory() -> str:
    return str(uuid.uuid4())


def _has_replanner_role(agent_name: str) -> bool:
    from agents.registry import get_role

    return get_role(agent_name) == "replanner"


# ---------------------------------------------------------------------------
# GraphMutation — the diff produced by every TaskGraph method
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class StatusChange:
    """One task's status transition.

    ``reason`` populates ``failure_reason`` when the status is terminal.
    The ``replan_requested:`` prefix for REQUEST_REPLAN is applied by
    ``TaskGraph.apply`` (matching the SQL behavior in
    ``tasks_sql.set_status``), so in-memory and DB renderings agree.
    """

    task_id: str
    new_status: TaskStatus
    reason: str | None = None


@dataclass(frozen=True)
class TaskInsert:
    """A fully-formed ``Task`` ready to insert into the graph + DB."""

    task: Task


@dataclass(frozen=True)
class DepRewire:
    """Replace ``old_dep_id`` with ``new_dep_ids`` on every task in ``affected_task_ids``."""

    old_dep_id: str
    new_dep_ids: tuple[str, ...]
    affected_task_ids: tuple[str, ...]


@dataclass(frozen=True)
class FailureReasonPatch:
    """Update ``failure_reason`` without touching status. Used for replanner
    → origin linkage (``finalize_replanned_origin``)."""

    task_id: str
    failure_reason: str


@dataclass(frozen=True)
class GraphMutation:
    """Aggregated diff returned by every ``TaskGraph`` method.

    The coordinator composes mutations across multiple method calls inside one
    handler, then hands the merged result to ``store.persist`` (one tx) and
    ``graph.apply`` (one in-memory update).
    """

    status_changes: tuple[StatusChange, ...] = ()
    inserts: tuple[TaskInsert, ...] = ()
    rewires: tuple[DepRewire, ...] = ()
    failure_reason_patches: tuple[FailureReasonPatch, ...] = ()

    @classmethod
    def empty(cls) -> "GraphMutation":
        return cls()

    def merge(self, other: "GraphMutation") -> "GraphMutation":
        return GraphMutation(
            status_changes=self.status_changes + other.status_changes,
            inserts=self.inserts + other.inserts,
            rewires=self.rewires + other.rewires,
            failure_reason_patches=self.failure_reason_patches + other.failure_reason_patches,
        )

    def is_empty(self) -> bool:
        return (
            not self.status_changes
            and not self.inserts
            and not self.rewires
            and not self.failure_reason_patches
        )


# ---------------------------------------------------------------------------
# Typed outcomes for multi-field returns
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ReplanSpawn:
    """Result of ``plan_request_replan``."""

    mutation: GraphMutation
    replanner_task: Task
    is_new: bool


@dataclass(frozen=True)
class ReplanApply:
    """Result of ``apply_replan``."""

    mutation: GraphMutation
    cancelled_ids: tuple[str, ...]
    cancelled_running_ids: tuple[str, ...]
    inserted_tasks: tuple[Task, ...]
    replanner_child_count: int


# ---------------------------------------------------------------------------
# TaskGraph
# ---------------------------------------------------------------------------


class TaskGraph:
    """Authoritative in-memory graph state + mutation rules."""

    def __init__(self, tasks: dict[str, Task] | None = None) -> None:
        self._tasks: dict[str, Task] = dict(tasks or {})

    # ---- accessors ------------------------------------------------------

    @property
    def tasks(self) -> dict[str, Task]:
        return self._tasks

    def get(self, task_id: str) -> Task | None:
        return self._tasks.get(task_id)

    def terminal_child_ids(self) -> list[str]:
        return [
            t.id
            for t in self._tasks.values()
            if t.parent_id is not None and t.status in TERMINAL_STATUSES
        ]

    def replace_all(self, tasks: Iterable[Task]) -> None:
        self._tasks = {t.id: t for t in tasks}

    # ---- pure rule methods (return GraphMutation) ----------------------

    def promote_on_done(self, task_id: str) -> GraphMutation:
        task = self._tasks.get(task_id)
        if task is None or task.status in TERMINAL_STATUSES:
            return GraphMutation.empty()
        changes: list[StatusChange] = [StatusChange(task_id, TaskStatus.DONE)]
        done_ids = {
            tid for tid, t in self._tasks.items() if t.status is TaskStatus.DONE
        }
        done_ids.add(task_id)
        for other in self._tasks.values():
            if other.status is not TaskStatus.PENDING:
                continue
            if task_id not in (other.deps or []):
                continue
            if all(d in done_ids for d in other.deps):
                changes.append(StatusChange(other.id, TaskStatus.READY))
        return GraphMutation(status_changes=tuple(changes))

    def mark_expanded(self, task_id: str) -> GraphMutation:
        if self._tasks.get(task_id) is None:
            return GraphMutation.empty()
        return GraphMutation(status_changes=(StatusChange(task_id, TaskStatus.EXPANDED),))

    def fail(self, task_id: str, reason: str) -> GraphMutation:
        task = self._tasks.get(task_id)
        if task is None or task.status in _FULLY_TERMINAL:
            return GraphMutation.empty()
        return GraphMutation(
            status_changes=(StatusChange(task_id, TaskStatus.FAILED, reason=reason),)
        )

    def cancel(self, task_id: str, reason: str) -> GraphMutation:
        task = self._tasks.get(task_id)
        if task is None or task.status in _FULLY_TERMINAL:
            return GraphMutation.empty()
        return GraphMutation(
            status_changes=(StatusChange(task_id, TaskStatus.CANCELLED, reason=reason),)
        )

    def compute_cancel_cascade(self, root_task_id: str) -> set[str]:
        live_ids = {
            tid for tid, t in self._tasks.items() if t.status not in TERMINAL_STATUSES
        }
        children: dict[str, list[str]] = defaultdict(list)
        dependents: dict[str, list[str]] = defaultdict(list)
        for t in self._tasks.values():
            if t.status in TERMINAL_STATUSES:
                continue
            if t.parent_id:
                children[t.parent_id].append(t.id)
            for dep in t.deps or []:
                dependents[dep].append(t.id)
        cascaded: set[str] = set()
        queue: deque[str] = deque([root_task_id])
        while queue:
            cur = queue.popleft()
            for nxt in children.get(cur, []) + dependents.get(cur, []):
                if nxt in live_ids and nxt not in cascaded:
                    cascaded.add(nxt)
                    queue.append(nxt)
        cascaded.discard(root_task_id)
        return cascaded

    def cancel_cascade(
        self, root_task_id: str
    ) -> tuple[GraphMutation, tuple[str, ...]]:
        cascaded = self.compute_cancel_cascade(root_task_id)
        if not cascaded:
            return GraphMutation.empty(), ()
        ordered = tuple(sorted(cascaded))
        reason = f"cascaded from {root_task_id}"
        changes = tuple(
            StatusChange(tid, TaskStatus.CANCELLED, reason=reason) for tid in ordered
        )
        return GraphMutation(status_changes=changes), ordered

    def find_promotable_parent(self, child_id: str) -> str | None:
        child = self._tasks.get(child_id)
        if child is None or child.parent_id is None:
            return None
        parent = self._tasks.get(child.parent_id)
        if parent is None or parent.status is not TaskStatus.EXPANDED:
            return None
        for t in self._tasks.values():
            if t.parent_id != parent.id:
                continue
            if t.status not in TERMINAL_STATUSES:
                return None
        return parent.id

    def plan_request_replan(
        self,
        *,
        task_id: str,
        reason: str,
        replanner_agent: str,
        replanner_id_factory: Callable[[], str] = _default_id_factory,
    ) -> ReplanSpawn:
        origin = self._tasks.get(task_id)
        if origin is None:
            raise GraphInvariantViolation(
                f"request_replan: task {task_id!r} not found"
            )
        if origin.status in _FULLY_TERMINAL:
            raise GraphInvariantViolation(
                f"request_replan: task {task_id} is terminal "
                f"({origin.status.value}); cannot replan"
            )

        # fired_by_task_id always points to the root origin to keep recovery
        # chains one-hop deep.
        root_origin_id = origin.fired_by_task_id or task_id

        # Idempotent per origin: reuse any live replanner already firing on
        # this origin. fired_by_task_id can also identify non-replanner
        # trigger tasks, so filter by role.
        for t in self._tasks.values():
            if t.status in TERMINAL_STATUSES:
                continue
            if t.fired_by_task_id != root_origin_id:
                continue
            if not _has_replanner_role(t.agent):
                continue
            return ReplanSpawn(
                mutation=GraphMutation.empty(), replanner_task=t, is_new=False
            )

        # replace_dependency invariant: anyone depending on task_id must be
        # PENDING. A running/ready/terminal dependent signals a bug, not a
        # recoverable race.
        rewire_targets: list[str] = []
        for t in self._tasks.values():
            if task_id not in (t.deps or []):
                continue
            if t.status is not TaskStatus.PENDING:
                raise GraphInvariantViolation(
                    "replan dependency invariant violated: "
                    f"tasks depending on {task_id!r} must be pending; "
                    f"found {t.id}:{t.status.value}"
                )
            rewire_targets.append(t.id)

        replanner_id = replanner_id_factory()
        task_text = (
            f"Replan: {origin.agent} failed on task {task_id}: {reason}"
        )
        spec = TaskSpec(
            goal=f"Replan failed task {task_id}.",
            detail=task_text,
            acceptance_criteria=(
                "Submit exactly one corrective submit_replan payload with at "
                "least one new task and explicit cancel_ids."
            ),
        )
        replanner = Task(
            id=replanner_id,
            team_run_id=origin.team_run_id,
            spec=spec,
            agent=replanner_agent,
            status=TaskStatus.READY,
            scope_paths=list(origin.scope_paths),
            parent_id=origin.parent_id,
            root_id=origin.root_id or "",
            depth=origin.depth or 0,
            fired_by_task_id=root_origin_id,
        )

        status_changes: tuple[StatusChange, ...] = ()
        if origin.status is not TaskStatus.REQUEST_REPLAN:
            status_changes = (
                StatusChange(task_id, TaskStatus.REQUEST_REPLAN, reason=reason),
            )
        rewires: tuple[DepRewire, ...] = ()
        if rewire_targets:
            rewires = (
                DepRewire(
                    old_dep_id=task_id,
                    new_dep_ids=(replanner_id,),
                    affected_task_ids=tuple(rewire_targets),
                ),
            )
        mutation = GraphMutation(
            status_changes=status_changes,
            inserts=(TaskInsert(replanner),),
            rewires=rewires,
        )
        return ReplanSpawn(mutation=mutation, replanner_task=replanner, is_new=True)

    def apply_replan(
        self,
        *,
        replan_task_id: str,
        add_tasks: list[TaskDefinition],
        cancel_ids: list[str],
        new_task_id_factory: Callable[[], str] = _default_id_factory,
    ) -> ReplanApply:
        replanner = self._tasks.get(replan_task_id)
        if replanner is None:
            raise GraphInvariantViolation(
                f"apply_replan: replanner {replan_task_id!r} not found"
            )

        # Compute cancellation set (root + cascade) and snapshot RUNNING
        # members BEFORE building mutation so the coordinator can reach live
        # worker tasks.
        root_cancel_ids = [
            cid for cid in cancel_ids if self._tasks.get(cid) is not None
        ]
        cancelled: set[str] = set()
        running_cancelled: set[str] = set()
        status_changes: list[StatusChange] = []
        for cid in root_cancel_ids:
            target = self._tasks.get(cid)
            if target is None or target.status in _FULLY_TERMINAL:
                continue
            if cid in cancelled:
                continue
            if target.status is TaskStatus.RUNNING:
                running_cancelled.add(cid)
            cancelled.add(cid)
            status_changes.append(
                StatusChange(
                    cid,
                    TaskStatus.CANCELLED,
                    reason=f"cancelled_by_replan_{replan_task_id}",
                )
            )
            for sub in sorted(self.compute_cancel_cascade(cid)):
                if sub in cancelled:
                    continue
                sub_task = self._tasks.get(sub)
                if sub_task is None:
                    continue
                if sub_task.status is TaskStatus.RUNNING:
                    running_cancelled.add(sub)
                cancelled.add(sub)
                status_changes.append(
                    StatusChange(
                        sub,
                        TaskStatus.CANCELLED,
                        reason=f"cascaded from {cid}",
                    )
                )

        inserts: list[TaskInsert] = []
        inserted_tasks: list[Task] = []
        replanner_child_count = 0
        if add_tasks:
            done_ids = {
                tid
                for tid, t in self._tasks.items()
                if t.status is TaskStatus.DONE
            }
            for spec_def in add_tasks:
                parent_id = spec_def.parent_id or replan_task_id
                parent = self._tasks.get(parent_id) or replanner
                # Replan children share the replanner's depth (same-depth
                # insert — replanning is not a new planning layer).
                child_depth = parent.depth or 0
                if parent_id == replan_task_id:
                    replanner_child_count += 1
                deps = list(spec_def.deps or [])
                initial_status = (
                    TaskStatus.READY
                    if all(d in done_ids for d in deps)
                    else TaskStatus.PENDING
                )
                new_task = Task(
                    id=spec_def.id or new_task_id_factory(),
                    team_run_id=replanner.team_run_id,
                    spec=spec_def.spec,
                    agent=spec_def.agent,
                    status=initial_status,
                    deps=deps,
                    scope_paths=list(spec_def.scope_paths or []),
                    parent_id=parent_id,
                    root_id=replanner.root_id or replan_task_id,
                    depth=child_depth,
                )
                inserts.append(TaskInsert(new_task))
                inserted_tasks.append(new_task)

        mutation = GraphMutation(
            status_changes=tuple(status_changes),
            inserts=tuple(inserts),
        )
        return ReplanApply(
            mutation=mutation,
            cancelled_ids=tuple(sorted(cancelled)),
            cancelled_running_ids=tuple(sorted(running_cancelled)),
            inserted_tasks=tuple(inserted_tasks),
            replanner_child_count=replanner_child_count,
        )

    def finalize_replanned_origin(self, replanner_task_id: str) -> GraphMutation:
        replanner = self._tasks.get(replanner_task_id)
        if replanner is None or replanner.fired_by_task_id is None:
            return GraphMutation.empty()
        origin = self._tasks.get(replanner.fired_by_task_id)
        if origin is None or origin.status is not TaskStatus.REQUEST_REPLAN:
            return GraphMutation.empty()
        return GraphMutation(
            failure_reason_patches=(
                FailureReasonPatch(
                    origin.id, f"replanned_by:{replanner_task_id}"
                ),
            )
        )

    def insert_plan_children(
        self,
        *,
        parent_id: str,
        specs: list[TaskDefinition],
        new_task_id_factory: Callable[[], str] = _default_id_factory,
    ) -> GraphMutation:
        if not specs:
            return GraphMutation.empty()
        parent = self._tasks.get(parent_id)
        if parent is None:
            raise GraphInvariantViolation(
                f"insert_plan_children: parent {parent_id!r} not found"
            )
        done_ids = {
            tid for tid, t in self._tasks.items() if t.status is TaskStatus.DONE
        }
        child_depth = (parent.depth or 0) + 1
        inserts: list[TaskInsert] = []
        for spec_def in specs:
            deps = list(spec_def.deps or [])
            initial_status = (
                TaskStatus.READY
                if all(d in done_ids for d in deps)
                else TaskStatus.PENDING
            )
            new_task = Task(
                id=spec_def.id or new_task_id_factory(),
                team_run_id=parent.team_run_id,
                spec=spec_def.spec,
                agent=spec_def.agent,
                status=initial_status,
                deps=deps,
                scope_paths=list(spec_def.scope_paths or []),
                parent_id=parent_id,
                root_id=parent.root_id or parent.id,
                depth=child_depth,
            )
            inserts.append(TaskInsert(new_task))
        return GraphMutation(inserts=tuple(inserts))

    # ---- apply ---------------------------------------------------------

    def apply(self, mutation: GraphMutation) -> None:
        for change in mutation.status_changes:
            task = self._tasks.get(change.task_id)
            if task is None:
                continue
            self._apply_status_change(task, change)
        for insert in mutation.inserts:
            self._tasks[insert.task.id] = insert.task
        for rewire in mutation.rewires:
            for tid in rewire.affected_task_ids:
                task = self._tasks.get(tid)
                if task is None:
                    continue
                new_deps = [d for d in (task.deps or []) if d != rewire.old_dep_id]
                for new_dep in rewire.new_dep_ids:
                    if new_dep not in new_deps:
                        new_deps.append(new_dep)
                task.deps = new_deps
                # Defense-in-depth (matches tasks_sql.replace_dependency): the
                # invariant guarantees affected tasks are PENDING, so these
                # are usually None already.
                task.started_at = None
                task.agent_run_id = None
        for patch in mutation.failure_reason_patches:
            task = self._tasks.get(patch.task_id)
            if task is None:
                continue
            task.failure_reason = patch.failure_reason

    def _apply_status_change(self, task: Task, change: StatusChange) -> None:
        task.status = change.new_status
        # Terminal transitions stamp finished_at (matches tasks_sql.set_status).
        # Idempotent: don't overwrite an existing stamp.
        if change.new_status in TERMINAL_STATUSES and task.finished_at is None:
            task.finished_at = _utcnow()
        if change.reason is not None:
            if change.new_status is TaskStatus.REQUEST_REPLAN:
                task.failure_reason = f"replan_requested: {change.reason}"
            else:
                task.failure_reason = change.reason


__all__ = [
    "DepRewire",
    "FailureReasonPatch",
    "GraphMutation",
    "ReplanApply",
    "ReplanSpawn",
    "StatusChange",
    "TaskGraph",
    "TaskInsert",
]

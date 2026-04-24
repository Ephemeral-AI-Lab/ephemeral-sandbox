"""Shared replan validation rules.

The submission tool validation path (pre-submission, inside
``_validate_submit_replan_input``) and the plan expander (at-apply, inside
``PlanExpander.apply_replan``) enforce the same layer-restricted rules.
They live here so the two callers cannot drift apart.

Scope is deliberately narrow: a replanner may only author direct children
under itself. That makes the replanner the recovery gate for downstream tasks
rewired from the failed worker. It can cancel its direct siblings; cascade
handles their descendants and dependents.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Iterable

from team.core.models import TERMINAL_STATUSES

ALLOWED_REPLAN_DEP_STATUSES = {"done", "ready", "pending"}


@dataclass
class ReplanValidationResult:
    errors: list[str] = field(default_factory=list)
    origin_task_id: str | None = None
    all_cancelled_ids: set[str] = field(default_factory=set)
    allowed_existing_dep_ids: set[str] = field(default_factory=set)


def _active_tasks(graph: dict[str, Any]) -> dict[str, Any]:
    return {
        tid: t
        for tid, t in graph.items()
        if getattr(t, "status", None) not in TERMINAL_STATUSES
    }


def _cascade_ids_for_cancel_root(
    graph: dict[str, Any],
    cancel_root_id: str,
) -> set[str]:
    active = _active_tasks(graph)
    children_by_parent: dict[str, list[str]] = {}
    dependents_by_task_id: dict[str, list[str]] = {}
    for tid, task in active.items():
        parent_id = getattr(task, "parent_id", None)
        if parent_id:
            children_by_parent.setdefault(str(parent_id), []).append(tid)
        for dep_id in getattr(task, "deps", []) or []:
            dependents_by_task_id.setdefault(str(dep_id), []).append(tid)

    cascaded: set[str] = set()
    queue = [cancel_root_id]
    while queue:
        current = queue.pop(0)
        for child_id in children_by_parent.get(current, []):
            if child_id not in cascaded:
                cascaded.add(child_id)
                queue.append(child_id)
        for dependent_id in dependents_by_task_id.get(current, []):
            if dependent_id in active and dependent_id not in cascaded:
                cascaded.add(dependent_id)
                queue.append(dependent_id)
    cascaded.discard(cancel_root_id)
    return cascaded


def _status_value(status: Any) -> Any:
    return getattr(status, "value", status)


def _depends_on_any(
    graph: dict[str, Any],
    *,
    task_id: str,
    blocked_dep_ids: set[str],
) -> bool:
    """Return True when task_id's dependency chain reaches a blocked dep."""
    task = graph.get(task_id)
    stack = [str(dep_id) for dep_id in getattr(task, "deps", []) or []]
    seen: set[str] = set()
    while stack:
        dep_id = stack.pop()
        if dep_id in blocked_dep_ids:
            return True
        if dep_id in seen:
            continue
        seen.add(dep_id)
        dep_task = graph.get(dep_id)
        if dep_task is not None:
            stack.extend(str(next_dep) for next_dep in getattr(dep_task, "deps", []) or [])
    return False


def validate_replan_rules(
    *,
    graph: dict[str, Any] | None,
    replan_task_id: str,
    cancel_ids: Iterable[str],
) -> ReplanValidationResult:
    """Validate replan cancel targets and compute dep/cancel sets.

    New task specs carry no free-form ``parent_id``; callers stamp every new
    task as a direct child of the replanner. This validator enforces the
    cancel-side rules and exposes ``allowed_existing_dep_ids`` for validating
    new-task dependencies.
    """
    result = ReplanValidationResult()
    if graph is None:
        result.errors.append("submit_replan requires the current task graph for validation")
        return result

    replanner = graph.get(replan_task_id)
    if replanner is None:
        result.errors.append(f"replanner task '{replan_task_id}' not found in graph")
        return result
    origin_task_id = (
        getattr(replanner, "fired_by_task_id", None) if replanner is not None else None
    )
    replanner_parent_id = (
        getattr(replanner, "parent_id", None) if replanner is not None else None
    )
    result.origin_task_id = origin_task_id

    cancel_id_list = list(cancel_ids)
    cancel_id_set = set(cancel_id_list)

    if replan_task_id in cancel_id_set:
        result.errors.append("replanner cannot cancel itself")
    if origin_task_id and origin_task_id in cancel_id_set:
        result.errors.append("replanner cannot cancel the original request_replan task")

    all_cancelled = set(cancel_id_set)
    for cid in cancel_id_list:
        target = graph.get(cid)
        if target is None:
            result.errors.append(f"cancel target '{cid}' not found in graph")
            continue
        if cid == replan_task_id or cid == origin_task_id:
            continue
        target_parent = getattr(target, "parent_id", None)
        if target_parent != replanner_parent_id:
            result.errors.append(
                f"cancel target '{cid}' is not a direct sibling of the replanner "
                f"(replanner.parent_id={replanner_parent_id!r}, "
                f"target.parent_id={target_parent!r}); replanners may only "
                f"cancel their direct siblings"
            )
        status = getattr(target, "status", None)
        if status is not None and status in TERMINAL_STATUSES:
            result.errors.append(
                f"cancel target '{cid}' is {_status_value(status)}; cannot cancel"
            )
        all_cancelled.update(_cascade_ids_for_cancel_root(graph, cid))
    result.all_cancelled_ids = all_cancelled

    excluded_dep_ids: set[str] = {replan_task_id}
    if origin_task_id:
        excluded_dep_ids.add(origin_task_id)
    allowed_existing_dep_ids: set[str] = set()
    for tid, task in graph.items():
        if tid in all_cancelled or tid in excluded_dep_ids:
            continue
        if _depends_on_any(
            graph,
            task_id=tid,
            blocked_dep_ids=excluded_dep_ids,
        ):
            continue
        status_value = _status_value(getattr(task, "status", None))
        if status_value in ALLOWED_REPLAN_DEP_STATUSES:
            allowed_existing_dep_ids.add(tid)
    result.allowed_existing_dep_ids = allowed_existing_dep_ids

    return result

from __future__ import annotations

from typing import TYPE_CHECKING

from team.errors import BudgetExceeded, InvalidPlan
from team.models import WorkItem, WorkItemKind, WorkItemStatus, _utcnow
from team.persistence.events import make_work_item_added, make_work_item_status, work_item_to_dict
from team.runtime.dispatcher_mutation_ops import cascade_cancel

if TYPE_CHECKING:
    from team.runtime.dispatcher import Dispatcher


def should_reattach_failed_verifier(failed_wi: WorkItem) -> bool:
    from team.builtins import VALIDATOR
    return failed_wi.agent_name == VALIDATOR and failed_wi.status == WorkItemStatus.FAILED


def build_replan_verifier_deps(
    dispatcher: "Dispatcher",
    failed_wi: WorkItem,
    *,
    new_item_ids: list[str],
    cancelled_ids: set[str],
) -> list[str]:
    deps: list[str] = []
    seen: set[str] = set()
    for dep_id in [*failed_wi.deps, *new_item_ids]:
        if dep_id in seen or dep_id in cancelled_ids:
            continue
        dep = dispatcher.graph.get(dep_id)
        if dep is not None and dep.status == WorkItemStatus.CANCELLED:
            continue
        deps.append(dep_id)
        seen.add(dep_id)
    return deps


async def apply_replan(
    dispatcher: "Dispatcher",
    *,
    replan_wi_id: str,
    add_specs: list[dict],
    cancel_ids: list[str],
    target_depth: int,
    target_parent_id: str | None,
    target_root_id: str,
) -> dict[str, int]:
    from team.models import Briefing
    async with dispatcher.lock:
        replan_wi = dispatcher.graph.get(replan_wi_id)
        if replan_wi is None:
            raise InvalidPlan(f"replanner work item {replan_wi_id} not found")
        failed_wi_id = replan_wi.replan_source_id or (replan_wi.payload or {}).get(
            "failed_work_item_id"
        )
        failed_wi = dispatcher.graph.get(failed_wi_id) if failed_wi_id else None
        if failed_wi is None:
            raise InvalidPlan(f"failed work item {failed_wi_id!r} not found")

        for cid in cancel_ids:
            wi = dispatcher.graph.get(cid)
            if wi is None:
                raise InvalidPlan(f"cancel target {cid} not found")
            if wi.parent_id != target_parent_id:
                raise InvalidPlan(
                    f"cancel target {cid} has parent {wi.parent_id!r}, "
                    f"but replan is scoped to parent {target_parent_id!r}"
                )
            if wi.status not in (WorkItemStatus.PENDING, WorkItemStatus.READY):
                raise InvalidPlan(
                    f"cancel target {cid} is {wi.status.value}; "
                    f"can only cancel PENDING or READY items"
                )

        local_to_new: dict[str, str] = {}
        for spec in add_specs:
            lid = spec.get("local_id")
            if lid:
                if lid in local_to_new:
                    raise InvalidPlan(f"duplicate local_id '{lid}'")
                local_to_new[lid] = dispatcher.new_id()
        new_items: list[WorkItem] = []
        for spec in add_specs:
            lid = spec.get("local_id")
            new_id = local_to_new.get(lid, dispatcher.new_id()) if lid else dispatcher.new_id()
            resolved_deps: list[str] = []
            for dep in spec.get("deps") or []:
                if dep in local_to_new:
                    resolved_deps.append(local_to_new[dep])
                elif dep in dispatcher.graph:
                    resolved_deps.append(dep)
                else:
                    raise InvalidPlan(f"dep '{dep}' not found")

            briefings = [Briefing(**b) for b in (spec.get("briefings") or [])]
            new_items.append(
                WorkItem(
                    id=new_id,
                    team_run_id=dispatcher.team_run_id,
                    agent_name=spec["agent_name"],
                    status=WorkItemStatus.PENDING,
                    kind=WorkItemKind(spec.get("kind", "atomic")),
                    deps=resolved_deps,
                    parent_id=target_parent_id,
                    root_id=target_root_id,
                    depth=target_depth,
                    local_id=lid,
                    payload=dict(spec.get("payload") or {}),
                    timeout_seconds=spec.get("timeout_seconds"),
                    briefings=briefings,
                )
            )
        if dispatcher.budget_state.work_items_used + len(new_items) > dispatcher.budgets.max_work_items:
            raise BudgetExceeded("max_work_items would be exceeded by replan")
        cancelled_set = set(cancel_ids)
        verifier_reset_deps: list[str] | None = None
        if should_reattach_failed_verifier(failed_wi):
            verifier_reset_deps = build_replan_verifier_deps(
                dispatcher,
                failed_wi,
                new_item_ids=[nwi.id for nwi in new_items],
                cancelled_ids=cancelled_set,
            )
        combined_adj: dict[str, list[str]] = {}
        for wi_id_key, wi in dispatcher.graph.items():
            if wi_id_key not in cancelled_set:
                combined_adj[wi_id_key] = list(wi.deps)
        for nwi in new_items:
            combined_adj[nwi.id] = list(nwi.deps)
        if verifier_reset_deps is not None:
            combined_adj[failed_wi.id] = list(verifier_reset_deps)

        visited: set[str] = set()
        on_stack: set[str] = set()
        def _has_cycle_from(node: str) -> bool:
            if node in on_stack:
                return True
            if node in visited:
                return False
            visited.add(node)
            on_stack.add(node)
            for nb in combined_adj.get(node, []):
                if _has_cycle_from(nb):
                    return True
            on_stack.discard(node)
            return False

        for start in combined_adj:
            if _has_cycle_from(start):
                raise InvalidPlan("replan would create a cycle in the combined graph")
        for cid in cancel_ids:
            wi = dispatcher.graph[cid]
            dispatcher._mark_cancelled(wi, f"cancelled_by_replan_{replan_wi_id}")
            cascade_cancel(dispatcher, cid)
        for nwi in new_items:
            dispatcher.graph[nwi.id] = nwi
            dispatcher.budget_state.work_items_used += 1
            dispatcher._emit(make_work_item_added(dispatcher.team_run_id, work_item_to_dict(nwi)))
        if verifier_reset_deps is not None:
            failed_wi.status = WorkItemStatus.PENDING
            failed_wi.deps = list(verifier_reset_deps)
            failed_wi.agent_run_id = None
            failed_wi.artifact_ref = None
            failed_wi.started_at = None
            failed_wi.finished_at = None
            failed_wi.failure_reason = None
            failed_wi.dep_artifacts = []
            dispatcher._emit(make_work_item_status(dispatcher.team_run_id, failed_wi.id, "pending"))
        if new_items:
            dispatcher._emit_budget()
        for nwi in new_items:
            if dispatcher._compute_readiness(nwi):
                dispatcher._promote_to_ready(nwi)
        if verifier_reset_deps is not None and dispatcher._compute_readiness(failed_wi):
            dispatcher._promote_to_ready(failed_wi)
        return {"added": len(new_items), "cancelled": len(cancel_ids)}


async def request_replan(
    dispatcher: "Dispatcher",
    *,
    wi_id: str,
    request: object,
) -> WorkItem:
    from team.builtins import TEAM_REPLANNER
    from team.models import ReplanRequest
    assert isinstance(request, ReplanRequest)
    async with dispatcher.lock:
        wi = dispatcher.graph[wi_id]
        if wi.status != WorkItemStatus.RUNNING:
            raise RuntimeError(f"replan: {wi_id} is {wi.status.value}, not RUNNING")
        if dispatcher.budget_state.replans_used >= dispatcher.budgets.max_replans_per_run:
            dispatcher._mark_failed(wi, f"replan_budget_exhausted: {request.reason}")
            cascade_cancel(dispatcher, wi_id)
            raise BudgetExceeded("max_replans_per_run reached")
        dispatcher._mark_failed(wi, f"replan_requested: {request.reason}")
        for other in list(dispatcher.graph.values()):
            if (
                other.parent_id == wi.parent_id
                and other.id != wi_id
                and other.status in (WorkItemStatus.PENDING, WorkItemStatus.READY)
            ):
                dispatcher._mark_cancelled(other, f"cancelled_by_replan_from_{wi_id}")
                cascade_cancel(dispatcher, other.id)
        cascade_cancel(dispatcher, wi_id)
        done_sibling_ids = [
            other.id
            for other in dispatcher.graph.values()
            if other.parent_id == wi.parent_id
            and other.id != wi_id
            and other.status == WorkItemStatus.DONE
        ]
        replanner_id = dispatcher.new_id()
        replanner = WorkItem(
            id=replanner_id,
            team_run_id=dispatcher.team_run_id,
            agent_name=TEAM_REPLANNER,
            status=WorkItemStatus.PENDING,
            kind=WorkItemKind.ATOMIC,
            deps=done_sibling_ids,
            parent_id=wi.parent_id,
            root_id=wi.root_id,
            depth=wi.depth,
            local_id=f"replan-from-{wi.local_id or wi_id}",
            payload={
                "failed_work_item_id": wi_id,
                "failed_agent": wi.agent_name,
                "failure_reason": request.reason,
                "failure_context": request.context,
                "suggestion": request.suggestion,
                "original_payload": wi.payload,
            },
            briefings=list(wi.briefings),
            replan_source_id=wi_id,
        )
        dispatcher.graph[replanner_id] = replanner
        dispatcher.budget_state.work_items_used += 1
        dispatcher.budget_state.replans_used += 1
        dispatcher._emit(make_work_item_added(dispatcher.team_run_id, work_item_to_dict(replanner)))
        dispatcher._emit_budget()
        if dispatcher._compute_readiness(replanner):
            dispatcher._promote_to_ready(replanner)
        return replanner

"""Plan validation — Phase A (structural, tool-call time) and Phase B (Dispatcher)."""

from __future__ import annotations

from typing import Callable, Iterator

from agents.registry import get_definition as _get_definition

from team.errors import InvalidPlan
from team.models import Plan, WorkItem, WorkItemKind, WorkItemSpec, WorkItemStatus

Issue = dict[str, str]


def _agent_exists(agent_name: str) -> bool:
    return _get_definition(agent_name) is not None


def validate_plan_phase_a(plan: Plan, max_plan_size: int = 50) -> list[Issue]:
    """Pure-function structural validation."""
    issues: list[Issue] = []

    if len(plan.items) == 0:
        issues.append({"field": "items", "msg": "plan has no items"})
        return issues

    if len(plan.items) > max_plan_size:
        issues.append(
            {
                "field": "items",
                "msg": f"plan has {len(plan.items)} items, exceeds max_plan_size={max_plan_size}",
            }
        )
        return issues

    local_ids: set[str] = set()
    for idx, item in enumerate(plan.items):
        # local_id uniqueness
        if item.local_id is not None:
            if item.local_id in local_ids:
                issues.append(
                    {"field": f"items[{idx}].local_id", "msg": f"duplicate local_id '{item.local_id}'"}
                )
            local_ids.add(item.local_id)
        # agent existence
        if not item.agent_name:
            issues.append({"field": f"items[{idx}].agent_name", "msg": "agent_name is required"})
        elif not _agent_exists(item.agent_name):
            issues.append(
                {"field": f"items[{idx}].agent_name", "msg": f"unknown agent '{item.agent_name}'"}
            )

    # Dep refs + cycle check on internal subgraph.
    # Every item gets a node key (local_id or synthetic idx) so cycles
    # involving items without an explicit local_id are still detected.
    def _node_key(idx: int, item: "WorkItemSpec") -> str:
        return item.local_id if item.local_id is not None else f"__idx_{idx}__"

    adj: dict[str, list[str]] = {_node_key(i, it): [] for i, it in enumerate(plan.items)}
    for idx, item in enumerate(plan.items):
        node = _node_key(idx, item)
        for dep in item.deps:
            if dep in local_ids:
                adj[node].append(dep)
            elif not isinstance(dep, str) or not dep:
                issues.append(
                    {"field": f"items[{idx}].deps", "msg": f"invalid dep reference: {dep!r}"}
                )

    if _has_cycle(adj):
        issues.append({"field": "items", "msg": "cycle detected in submitted Plan"})

    return issues


def _has_cycle(adj: dict[str, list[str]]) -> bool:
    """Iterative DFS cycle detection — safe for deep graphs."""
    WHITE, GRAY, BLACK = 0, 1, 2
    color: dict[str, int] = {k: WHITE for k in adj}

    for start in list(adj.keys()):
        if color[start] != WHITE:
            continue
        # stack entries: (node, iterator over its neighbors)
        stack: list[tuple[str, Iterator[str]]] = [(start, iter(adj.get(start, ())))]
        color[start] = GRAY
        while stack:
            node, it = stack[-1]
            nxt = next(it, None)
            if nxt is None:
                color[node] = BLACK
                stack.pop()
                continue
            c = color.get(nxt, WHITE)
            if c == GRAY:
                return True
            if c == WHITE:
                color[nxt] = GRAY
                stack.append((nxt, iter(adj.get(nxt, ()))))
    return False


def validate_plan_phase_b(
    existing_graph: dict[str, WorkItem],
    plan: Plan,
    team_run_id: str,
    parent_wi: WorkItem,
    *,
    new_id_factory: Callable[[], str],
    max_depth: int,
) -> list[WorkItem]:
    """Dispatcher-time re-check. Resolves local_ids, checks externals, depth, cycles."""
    if parent_wi.kind != WorkItemKind.EXPANDABLE:
        raise InvalidPlan(
            f"work item {parent_wi.id} is {parent_wi.kind.value}; only expandable items may submit a plan"
        )
    new_depth = parent_wi.depth + 1
    if new_depth > max_depth:
        raise InvalidPlan(f"plan would exceed max_depth={max_depth} (parent depth={parent_wi.depth})")

    # Re-check local_id uniqueness — Phase A may have been bypassed if a Plan
    # was constructed directly rather than via the submit_plan tool.
    seen_locals: set[str] = set()
    for item in plan.items:
        if item.local_id is None:
            continue
        if item.local_id in seen_locals:
            raise InvalidPlan(f"duplicate local_id '{item.local_id}'")
        seen_locals.add(item.local_id)

    local_to_new: dict[str, str] = {
        item.local_id: new_id_factory() for item in plan.items if item.local_id is not None
    }

    issues: list[str] = []
    new_items: list[WorkItem] = []
    for idx, spec in enumerate(plan.items):
        agent_def = _get_definition(spec.agent_name)
        if agent_def is not None:
            supported = getattr(agent_def, "supported_kinds", None) or ["atomic", "expandable"]
            if spec.kind.value not in supported:
                issues.append(
                    f"items[{idx}] agent '{spec.agent_name}' does not support kind '{spec.kind.value}' (supports: {supported})"
                )
                continue
        new_id: str = local_to_new[spec.local_id] if spec.local_id else new_id_factory()
        resolved_deps: list[str] = []
        for dep in spec.deps:
            if dep in local_to_new:
                resolved_deps.append(local_to_new[dep])
            else:
                target = existing_graph.get(dep)
                if target is None:
                    issues.append(f"items[{idx}] dep '{dep}' not found in team run {team_run_id}")
                    continue
                if target.team_run_id != team_run_id:
                    issues.append(f"items[{idx}] dep '{dep}' is cross-run (rejected)")
                    continue
                resolved_deps.append(dep)

        new_items.append(
            WorkItem(
                id=new_id,
                team_run_id=team_run_id,
                agent_name=spec.agent_name,
                status=WorkItemStatus.PENDING,
                deps=resolved_deps,
                parent_id=parent_wi.id,
                root_id=parent_wi.root_id or parent_wi.id,
                payload=dict(spec.payload),
                timeout_seconds=spec.timeout_seconds,
                depth=new_depth,
                kind=spec.kind,
            )
        )

    if issues:
        raise InvalidPlan("; ".join(issues))

    combined_adj: dict[str, list[str]] = {wi_id: list(wi.deps) for wi_id, wi in existing_graph.items()}
    for wi in new_items:
        combined_adj[wi.id] = list(wi.deps)
    if _has_cycle(combined_adj):
        raise InvalidPlan("combined graph would contain a cycle")

    return new_items

"""Plan validation — Phase A (structural, tool-call time) and Phase B (Dispatcher)."""

from __future__ import annotations

from typing import Any

from team.types import InvalidPlan, Plan, WorkItem, WorkItemSpec, WorkItemStatus

Issue = dict[str, str]


def _agent_exists(agent_name: str) -> bool:
    """Look up the agent registry lazily to avoid import cycles."""
    try:
        from agents.registry import get_definition
    except Exception:  # pragma: no cover — fallback for sandboxed tests
        return True
    return get_definition(agent_name) is not None


def validate_plan_phase_a(plan: Plan, max_plan_size: int = 50) -> list[Issue]:
    """Pure-function structural validation.

    Checks:
      1. Size limit.
      2. ``local_id`` uniqueness.
      3. Every ``agent_name`` exists in the registry (if available).
      4. Every dep either references a local_id in this Plan or looks like an
         external work_item_id string (existence deferred to Phase B).
      5. No cycles inside the submitted subgraph.
    """
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

    # local_id uniqueness
    local_ids: set[str] = set()
    for idx, item in enumerate(plan.items):
        if item.local_id is None:
            continue
        if item.local_id in local_ids:
            issues.append(
                {"field": f"items[{idx}].local_id", "msg": f"duplicate local_id '{item.local_id}'"}
            )
        local_ids.add(item.local_id)

    # Agent existence
    for idx, item in enumerate(plan.items):
        if not item.agent_name:
            issues.append({"field": f"items[{idx}].agent_name", "msg": "agent_name is required"})
            continue
        if not _agent_exists(item.agent_name):
            issues.append(
                {
                    "field": f"items[{idx}].agent_name",
                    "msg": f"unknown agent '{item.agent_name}'",
                }
            )

    # Dep resolution + cycle detection on internal subgraph
    # Build adjacency using local_ids only (external deps are opaque for cycle checks).
    adj: dict[str, list[str]] = {lid: [] for lid in local_ids}
    for idx, item in enumerate(plan.items):
        if item.local_id is None:
            continue
        for dep in item.deps:
            if dep in local_ids:
                adj[item.local_id].append(dep)
            # external dep shape: non-empty string, deferred to Phase B
            elif not isinstance(dep, str) or not dep:
                issues.append(
                    {"field": f"items[{idx}].deps", "msg": f"invalid dep reference: {dep!r}"}
                )

    if _has_cycle(adj):
        issues.append({"field": "items", "msg": "cycle detected in submitted Plan"})

    return issues


def _has_cycle(adj: dict[str, list[str]]) -> bool:
    WHITE, GRAY, BLACK = 0, 1, 2
    color: dict[str, int] = {k: WHITE for k in adj}

    def dfs(node: str) -> bool:
        color[node] = GRAY
        for nxt in adj.get(node, ()):
            if color.get(nxt, WHITE) == GRAY:
                return True
            if color.get(nxt, WHITE) == WHITE and dfs(nxt):
                return True
        color[node] = BLACK
        return False

    return any(color[n] == WHITE and dfs(n) for n in adj)


def validate_plan_phase_b(
    existing_graph: dict[str, WorkItem],
    plan: Plan,
    team_run_id: str,
    parent_wi: WorkItem,
    *,
    new_id_factory,
    max_depth: int,
) -> list[WorkItem]:
    """Dispatcher-time re-check. Resolves local_ids to real work_item_ids,
    checks cross-run/dangling external refs, combined-graph cycles, and depth.
    Returns a list of fully-formed WorkItems ready to insert. Raises InvalidPlan on failure.
    """
    issues: list[str] = []

    # Resolve local_ids to fresh work_item_ids up front
    local_to_new: dict[str, str] = {}
    for item in plan.items:
        if item.local_id is not None:
            local_to_new[item.local_id] = new_id_factory()

    new_items: list[WorkItem] = []
    new_depth = parent_wi.depth + 1
    if new_depth > max_depth:
        raise InvalidPlan(f"plan would exceed max_depth={max_depth} (parent depth={parent_wi.depth})")

    for idx, spec in enumerate(plan.items):
        new_id = local_to_new.get(spec.local_id) if spec.local_id else new_id_factory()
        resolved_deps: list[str] = []
        for dep in spec.deps:
            if dep in local_to_new:
                resolved_deps.append(local_to_new[dep])
            else:
                # External dep — must exist in the same TeamRun's graph.
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
            )
        )

    if issues:
        raise InvalidPlan("; ".join(issues))

    # Combined graph cycle check: treat (existing + new) as adjacency and DFS.
    combined_adj: dict[str, list[str]] = {}
    for wi_id, wi in existing_graph.items():
        combined_adj[wi_id] = list(wi.deps)
    for wi in new_items:
        combined_adj[wi.id] = list(wi.deps)

    if _has_cycle(combined_adj):
        raise InvalidPlan("combined graph would contain a cycle")

    return new_items

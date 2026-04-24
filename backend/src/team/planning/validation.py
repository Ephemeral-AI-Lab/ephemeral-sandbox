"""Plan validation — single-pass structural, agent-resolution, cycle detection."""

from __future__ import annotations

from typing import Callable, Iterator

from agents.registry import get_definition as _get_definition, has_role as _has_role

from team.models import Plan, TaskDefinition

Issue = dict[str, str]

# Type alias for pluggable plan validators.
PlanItemValidator = Callable[[list[TaskDefinition]], list[Issue]]


def _agent_exists(agent_name: str) -> bool:
    return _get_definition(agent_name) is not None


def _is_validator(agent_name: str) -> bool:
    """Check whether *agent_name* has the reviewer role."""
    return _has_role(agent_name, "reviewer")


def _validator_count(items: list[TaskDefinition]) -> int:
    return sum(1 for item in items if _is_validator(item.agent))


def _validator_policy_issues(
    items: list[TaskDefinition],
) -> list[Issue]:
    issues: list[Issue] = []
    validator_count = _validator_count(items)
    if validator_count > 2:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    f"plan has {validator_count} validator tasks; submitted plans may have at most "
                    "2"
                ),
            }
        )
    return issues


def _terminal_non_validator_leaf_ids(items: list[TaskDefinition]) -> set[str]:
    downstream_ids = {dep for item in items for dep in item.deps if dep}
    return {
        item.id
        for item in items
        if item.id and not _is_validator(item.agent) and item.id not in downstream_ids
    }


def _validator_dependency_issues(items: list[TaskDefinition]) -> list[Issue]:
    issues: list[Issue] = []
    terminal_leaf_ids = _terminal_non_validator_leaf_ids(items)
    downstream_ids = {dep for item in items for dep in item.deps if dep}
    for idx, item in enumerate(items):
        if not _is_validator(item.agent):
            continue
        if not item.deps:
            issues.append(
                {
                    "field": f"tasks[{idx}].deps",
                    "msg": "validator tasks must depend on at least one upstream sibling",
                }
            )
            continue
        is_terminal = not item.id or item.id not in downstream_ids
        if not is_terminal or not terminal_leaf_ids:
            continue
        missing = sorted(terminal_leaf_ids.difference(item.deps))
        if missing:
            issues.append(
                {
                    "field": f"tasks[{idx}].deps",
                    "msg": (
                        "terminal validator must depend on every terminal non-validator sibling "
                        f"(missing: {', '.join(missing)})"
                    ),
                }
            )
    return issues


def validate_plan(
    plan: Plan,
    max_plan_size: int = 50,
    *,
    extra_validators: list[PlanItemValidator] | None = None,
) -> list[Issue]:
    """Single-pass structural validation: structural checks, agent resolution, cycle detection."""
    issues: list[Issue] = []

    if len(plan.tasks) == 0:
        issues.append({"field": "tasks", "msg": "plan has no tasks"})
        return issues

    if len(plan.tasks) > max_plan_size:
        issues.append(
            {
                "field": "tasks",
                "msg": f"plan has {len(plan.tasks)} tasks, exceeds max_plan_size={max_plan_size}",
            }
        )
        return issues

    issues.extend(_validator_policy_issues(plan.tasks))
    issues.extend(_validator_dependency_issues(plan.tasks))

    task_ids: set[str] = set()
    for idx, item in enumerate(plan.tasks):
        # id is required
        if not item.id:
            issues.append(
                {"field": f"tasks[{idx}].id", "msg": "task id is required (must be non-empty)"}
            )
        elif item.id in task_ids:
            issues.append(
                {"field": f"tasks[{idx}].id", "msg": f"duplicate task id '{item.id}'"}
            )
        else:
            task_ids.add(item.id)
        # agent existence
        if not item.agent:
            issues.append({"field": f"tasks[{idx}].agent", "msg": "agent is required"})
        elif not _agent_exists(item.agent):
            issues.append(
                {"field": f"tasks[{idx}].agent", "msg": f"unknown agent '{item.agent}'"}
            )
        else:
            agent_def = _get_definition(item.agent)
            if agent_def is not None and agent_def.agent_type != "agent":
                issues.append(
                    {
                        "field": f"tasks[{idx}].agent",
                        "msg": (
                            f"submitted plans cannot target {getattr(agent_def, 'agent_type', 'agent')!r}-typed "
                            f"agent '{item.agent}'; only team-facing agents are valid plan targets"
                        ),
                    }
                )
            if agent_def is not None and agent_def.role == "replanner":
                issues.append(
                    {
                        "field": f"tasks[{idx}].agent",
                        "msg": (
                            f"submitted plans cannot include replanner agent '{item.agent}'; "
                            "replanners are spawned reactively via request_replan, not planned"
                        ),
                    }
                )

        # scope_paths is required
        if not item.scope_paths:
            issues.append(
                {"field": f"tasks[{idx}].scope_paths", "msg": "scope_paths is required (at least one path)"}
            )

    # Dep refs + cycle check.
    adj: dict[str, list[str]] = {
        (it.id if it.id else f"__idx_{i}__"): [] for i, it in enumerate(plan.tasks)
    }
    for idx, item in enumerate(plan.tasks):
        node = item.id if item.id else f"__idx_{idx}__"
        for dep in item.deps:
            if not isinstance(dep, str) or not dep:
                issues.append(
                    {"field": f"tasks[{idx}].deps", "msg": f"invalid dep reference: {dep!r}"}
                )
            elif dep in task_ids:
                adj[node].append(dep)
            else:
                issues.append(
                    {
                        "field": f"tasks[{idx}].deps",
                        "msg": f"unknown dep reference '{dep}'",
                    }
                )

    if _has_cycle(adj):
        issues.append({"field": "tasks", "msg": "cycle detected in submitted Plan"})

    for validator in extra_validators or []:
        issues.extend(validator(plan.tasks))

    return issues


def _has_cycle(adj: dict[str, list[str]]) -> bool:
    """Iterative DFS cycle detection — safe for deep graphs."""
    WHITE, GRAY, BLACK = 0, 1, 2
    color: dict[str, int] = {k: WHITE for k in adj}

    for start in list(adj.keys()):
        if color[start] != WHITE:
            continue
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

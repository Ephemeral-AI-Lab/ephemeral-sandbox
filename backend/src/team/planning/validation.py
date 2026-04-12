"""Plan validation — single-pass structural, agent-resolution, cycle detection."""

from __future__ import annotations

from typing import Callable, Iterator

from agents.registry import get_definition as _get_definition, has_role as _has_role

from team.errors import InvalidPlan
from team.models import Plan, TaskSpec

Issue = dict[str, str]

# Type alias for pluggable plan validators.
PlanItemValidator = Callable[[list[TaskSpec]], list[Issue]]

def _agent_exists(agent_name: str) -> bool:
    return _get_definition(agent_name) is not None


def _is_validator(agent_name: str) -> bool:
    """Check whether *agent_name* has the reviewer role."""
    return _has_role(agent_name, "reviewer")


def _is_expandable(agent_name: str) -> bool:
    defn = _get_definition(agent_name)
    return defn is not None and defn.role == "planner"


def _validator_count(items: list[TaskSpec]) -> int:
    return sum(1 for item in items if _is_validator(item.agent))


def _concrete_execution_count(items: list[TaskSpec]) -> int:
    return sum(
        1
        for item in items
        if not _is_validator(item.agent) and not _is_expandable(item.agent)
    )


def _terminal_validator_count(items: list[TaskSpec]) -> int:
    downstream_ids = {dep for item in items for dep in item.deps if dep}
    return sum(
        1
        for item in items
        if _is_validator(item.agent) and item.id not in downstream_ids
    )


def _validator_policy_issues(
    items: list[TaskSpec],
    *,
    max_reviewers_per_plan: int | None = None,
    require_reviewer_for_plan_size: int | None = None,
) -> list[Issue]:
    issues: list[Issue] = []
    validator_count = _validator_count(items)
    effective_max = 2 if max_reviewers_per_plan is None else min(max_reviewers_per_plan, 2)
    require_threshold = (
        3 if require_reviewer_for_plan_size is None else min(require_reviewer_for_plan_size, 3)
    )
    if validator_count > effective_max:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    f"plan has {validator_count} validator tasks; submitted plans may have at most "
                    f"{effective_max}"
                ),
            }
        )
    if _concrete_execution_count(items) >= require_threshold and validator_count == 0:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    f"plans with {require_threshold} or more concrete non-planner tasks "
                    "must include at least one terminal validator"
                ),
            }
        )
    if validator_count == 0:
        return issues
    terminal_count = _terminal_validator_count(items)
    if terminal_count == 0:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    "plans with validator tasks must leave at least one validator as a "
                    "terminal end-of-chain guard"
                ),
            }
        )
    elif terminal_count > 1:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    "plans with validator tasks must keep exactly one validator as the "
                    "terminal end-of-chain guard"
                ),
            }
        )
    return issues


def _terminal_non_validator_leaf_ids(items: list[TaskSpec]) -> set[str]:
    downstream_ids = {dep for item in items for dep in item.deps if dep}
    return {
        item.id
        for item in items
        if item.id and not _is_validator(item.agent) and item.id not in downstream_ids
    }


def _validator_dependency_issues(items: list[TaskSpec]) -> list[Issue]:
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
    allow_empty: bool = False,
    known_external_deps: set[str] | None = None,
    max_reviewers_per_plan: int | None = None,
    require_reviewer_for_plan_size: int | None = None,
    extra_validators: list[PlanItemValidator] | None = None,
) -> list[Issue]:
    """Single-pass structural validation: structural checks, agent resolution, cycle detection."""
    issues: list[Issue] = []

    if len(plan.tasks) == 0:
        if allow_empty:
            return issues
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

    issues.extend(
        _validator_policy_issues(
            plan.tasks,
            max_reviewers_per_plan=max_reviewers_per_plan,
            require_reviewer_for_plan_size=require_reviewer_for_plan_size,
        )
    )
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

    # Dep refs + cycle check.
    adj: dict[str, list[str]] = {
        (it.id if it.id else f"__idx_{i}__"): [] for i, it in enumerate(plan.tasks)
    }
    for idx, item in enumerate(plan.tasks):
        node = item.id if item.id else f"__idx_{idx}__"
        for dep in item.deps:
            if dep in task_ids:
                adj[node].append(dep)
            elif not isinstance(dep, str) or not dep:
                issues.append(
                    {"field": f"tasks[{idx}].deps", "msg": f"invalid dep reference: {dep!r}"}
                )
            else:
                # dep is a non-empty string not in task_ids
                if known_external_deps is None or dep not in known_external_deps:
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

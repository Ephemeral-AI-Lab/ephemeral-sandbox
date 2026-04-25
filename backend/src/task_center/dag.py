"""DAG plan compiler — validate and compile a flat task list into a dep map.

A plan is a flat list of ``{id, deps}`` entries. Each entry's ``deps`` lists
its DIRECT dependencies; transitive deps are implicit via the graph.
"""

from __future__ import annotations

from typing import Any

from task_center.errors import PlanValidationError


def compile_dag(
    tasks: list[dict[str, Any]],
    task_specs: dict[str, dict[str, Any]],
) -> dict[str, frozenset[str]]:
    """Validate a flat DAG plan and compile it into a direct-dep map.

    Validations: non-empty inputs; entry shape; ids unique and present in
    ``task_specs``; deps reference known ids, no self-dep, no duplicates;
    no cycles.
    """
    if not isinstance(tasks, list) or len(tasks) == 0:
        raise PlanValidationError("tasks must be a non-empty list")
    if not isinstance(task_specs, dict) or len(task_specs) == 0:
        raise PlanValidationError("task_specs must be a non-empty dict")

    deps: dict[str, frozenset[str]] = {}

    for entry in tasks:
        if not isinstance(entry, dict):
            raise PlanValidationError(
                f"entries must be objects with 'id', got {entry!r}"
            )
        if "id" not in entry:
            raise PlanValidationError(f"entry missing 'id': {entry!r}")
        task_id = entry["id"]
        if not isinstance(task_id, str) or not task_id:
            raise PlanValidationError(
                f"entry 'id' must be a non-empty string, got {task_id!r}"
            )
        if task_id in deps:
            raise PlanValidationError(f"duplicate task id {task_id!r}")
        if task_id not in task_specs:
            raise PlanValidationError(
                f"task id {task_id!r} is not a key in task_specs"
            )

        raw_deps = entry.get("deps", [])
        if not isinstance(raw_deps, list):
            raise PlanValidationError(
                f"task {task_id!r}: 'deps' must be a list, got {type(raw_deps).__name__}"
            )
        if len(raw_deps) != len(set(raw_deps)):
            raise PlanValidationError(
                f"task {task_id!r}: 'deps' contains duplicate ids"
            )
        for dep_id in raw_deps:
            if not isinstance(dep_id, str):
                raise PlanValidationError(
                    f"task {task_id!r}: 'deps' entry must be a string, got {dep_id!r}"
                )
            if dep_id == task_id:
                raise PlanValidationError(
                    f"task {task_id!r}: 'deps' may not contain the entry's own id"
                )
        deps[task_id] = frozenset(raw_deps)

    # All deps must reference ids that appear as task entries.
    for task_id, dep_set in deps.items():
        for dep_id in dep_set:
            if dep_id not in deps:
                raise PlanValidationError(
                    f"task {task_id!r}: 'deps' references unknown id {dep_id!r}"
                )

    _check_no_cycles(deps)
    return deps


def sinks(deps: dict[str, frozenset[str]]) -> frozenset[str]:
    """Return the set of ids that no other task depends on."""
    has_dependent: set[str] = set()
    for dep_set in deps.values():
        has_dependent.update(dep_set)
    return frozenset(tid for tid in deps if tid not in has_dependent)


def _check_no_cycles(deps: dict[str, frozenset[str]]) -> None:
    WHITE, GRAY, BLACK = 0, 1, 2
    color: dict[str, int] = dict.fromkeys(deps, WHITE)

    def visit(tid: str, stack: list[str]) -> None:
        color[tid] = GRAY
        for dep in deps.get(tid, frozenset()):
            if color.get(dep) == GRAY:
                cycle_path = " -> ".join(stack[stack.index(dep):] + [dep])
                raise PlanValidationError(f"cycle detected in plan: {cycle_path}")
            if color.get(dep) == WHITE:
                visit(dep, stack + [dep])
        color[tid] = BLACK

    for tid in list(deps):
        if color[tid] == WHITE:
            visit(tid, [tid])

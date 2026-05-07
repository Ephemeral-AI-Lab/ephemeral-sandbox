"""Named predicates referenced by ``agent.md`` ``variants:`` entries.

Predicates are pure named functions registered in code. ``agent.md`` only
references them by id — there is no eval/dsl in the markdown.

The ``partial_plan_caller_ancestor`` predicate delegates to
:func:`task_center.mission.ancestry.has_partial_planned_caller_ancestor`.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.scope import ContextScope
from task_center.mission.ancestry import has_partial_planned_caller_ancestor


@dataclass(frozen=True, slots=True)
class ResolverContext:
    """Identity + dependency bundle handed to every predicate."""

    scope: ContextScope
    deps: ContextEngineDeps


PredicateFn = Callable[[ResolverContext], bool]


class PredicateRegistry:
    """Process-global registry. Tests use ``clear`` to start fresh."""

    _registry: dict[str, PredicateFn] = {}

    @classmethod
    def register(cls, name: str, fn: PredicateFn) -> None:
        cls._registry[name] = fn

    @classmethod
    def get(cls, name: str) -> PredicateFn:
        try:
            return cls._registry[name]
        except KeyError as exc:
            raise KeyError(
                f"Predicate {name!r} is not registered. "
                f"Known predicates: {sorted(cls._registry)!r}"
            ) from exc

    @classmethod
    def has(cls, name: str) -> bool:
        return name in cls._registry

    @classmethod
    def clear(cls) -> None:
        cls._registry.clear()


def _partial_plan_caller_ancestor(ctx: ResolverContext) -> bool:
    """Delegate to the canonical ancestry predicate."""
    return has_partial_planned_caller_ancestor(
        mission_id=ctx.scope.mission_id,
        mission_store=ctx.deps.mission_store,
        episode_store=ctx.deps.episode_store,
        attempt_store=ctx.deps.attempt_store,
        task_store=ctx.deps.task_store,
    )


def register_builtin_predicates() -> None:
    """Idempotent — safe to call from app startup."""
    PredicateRegistry.register(
        "partial_plan_caller_ancestor", _partial_plan_caller_ancestor
    )

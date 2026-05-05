"""Named predicates referenced by ``agent.md`` ``variants:`` entries.

Predicates are pure named functions registered in code. ``agent.md`` only
references them by id — there is no eval/dsl in the markdown.

The canonical ``partial_plan_caller_ancestor`` predicate is registered here
as a one-line shim around
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

    def has_partial_planned_caller_ancestor(self) -> bool:
        """Convenience delegate — keeps recipe / predicate / prehook on one
        function object so structural drift fails the test, not silently."""
        return has_partial_planned_caller_ancestor(
            request_id=self.scope.request_id,
            request_store=self.deps.request_store,
            segment_store=self.deps.segment_store,
            graph_store=self.deps.graph_store,
            task_store=self.deps.task_store,
        )


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
    def list_names(cls) -> list[str]:
        return sorted(cls._registry)

    @classmethod
    def clear(cls) -> None:
        cls._registry.clear()


def _partial_plan_caller_ancestor(ctx: ResolverContext) -> bool:
    """Shim → canonical ancestry function. Same kwargs, no extra logic."""
    return has_partial_planned_caller_ancestor(
        request_id=ctx.scope.request_id,
        request_store=ctx.deps.request_store,
        segment_store=ctx.deps.segment_store,
        graph_store=ctx.deps.graph_store,
        task_store=ctx.deps.task_store,
    )


def register_builtin_predicates() -> None:
    """Idempotent — safe to call from app startup."""
    PredicateRegistry.register(
        "partial_plan_caller_ancestor", _partial_plan_caller_ancestor
    )

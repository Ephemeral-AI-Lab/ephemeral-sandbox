"""Agent-variant routing — predicates and resolver.

Selects which concrete agent definition (e.g. ``executor_success_handoff``
vs ``executor_success_failure``) to spawn from a base agent name plus the
caller's :class:`ContextScope`, based on registered predicates.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import ClassVar

from agents import get_definition
from agents import AgentDefinition, AgentSelectionBlock, AgentVariant
from task_center.context_engine.core import (
    AgentDefinitionValidationError,
    ContextEngineDeps,
    MissingContextRecipeError,
)
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPriority,
)
from task_center.context_engine.scope import ContextScope
from task_center.mission.handler import nested_mission_depth


# ---------------------------------------------------------------------------
# Predicates
# ---------------------------------------------------------------------------

# Maximum nested-mission depth at which an executor profile still offers a
# handoff terminal. Above this, the leaf executor profile is selected (success
# + failure terminals only). Range-named predicates encode the threshold so
# renaming this constant does not require touching any frontmatter.
#
# Mutable so :class:`task_center.config.TaskCenterLifecycleConfig` can override
# it at startup via :func:`configure_max_handoff_depth`. Predicates read the
# current value at each invocation.
MAX_HANDOFF_DEPTH: int = 2


def configure_max_handoff_depth(value: int) -> None:
    """Set the runtime handoff-depth threshold (called by app startup).

    Calling this before ``register_builtin_predicates`` is fine — the
    predicates capture the module-level name at call time, not at
    registration.
    """
    global MAX_HANDOFF_DEPTH
    if value < 0:
        raise ValueError(
            f"max_handoff_depth must be >= 0, got {value!r}"
        )
    MAX_HANDOFF_DEPTH = value


@dataclass(frozen=True, slots=True)
class ResolverContext:
    """Identity + dependency bundle handed to every predicate."""

    scope: ContextScope
    deps: ContextEngineDeps


PredicateFn = Callable[[ResolverContext], bool]


class PredicateRegistry:
    """Process-global predicate registry. Tests use ``clear`` to start fresh."""

    _registry: ClassVar[dict[str, PredicateFn]] = {}

    @classmethod
    def register(cls, name: str, fn: PredicateFn) -> None:
        cls._registry[name] = fn

    @classmethod
    def get(cls, key: str) -> PredicateFn:
        try:
            return cls._registry[key]
        except KeyError as exc:
            raise KeyError(
                f"PredicateRegistry: {key!r} is not registered. "
                f"Known: {sorted(cls._registry)!r}"
            ) from exc

    @classmethod
    def has(cls, key: str) -> bool:
        return key in cls._registry

    @classmethod
    def list_ids(cls) -> list[str]:
        return sorted(cls._registry)

    @classmethod
    def clear(cls) -> None:
        cls._registry.clear()


def _depth(ctx: ResolverContext) -> int:
    """Return the nested-goal depth for ``ctx``.

    Scopes without a goal (e.g. the top-level entry executor) have no
    caller-trial ancestry by construction, so depth is zero.
    """
    goal_id = ctx.scope.mission_id
    if goal_id is None:
        return 0
    return nested_mission_depth(
        mission_id=goal_id,
        mission_store=ctx.deps.mission_store,
        episode_store=ctx.deps.episode_store,
        attempt_store=ctx.deps.attempt_store,
        task_store=ctx.deps.task_store,
    )


def _nested_goal_depth_within_handoff_range(ctx: ResolverContext) -> bool:
    """True when depth ≤ MAX_HANDOFF_DEPTH (executor may still hand off)."""
    return _depth(ctx) <= MAX_HANDOFF_DEPTH


def _nested_goal_depth_above_handoff_range(ctx: ResolverContext) -> bool:
    """True when depth > MAX_HANDOFF_DEPTH (leaf executor, no further handoff)."""
    return _depth(ctx) > MAX_HANDOFF_DEPTH


def _nested_goal_depth_gt_1(ctx: ResolverContext) -> bool:
    """True when depth > 1 — caller trial is itself inside another goal."""
    return _depth(ctx) > 1


def _always(ctx: ResolverContext) -> bool:
    """Total-coverage tail predicate — always True regardless of context."""
    return True


def register_builtin_predicates() -> None:
    """Idempotent — safe to call from app startup."""
    PredicateRegistry.register(
        "nested_mission_depth_within_handoff_range",
        _nested_goal_depth_within_handoff_range,
    )
    PredicateRegistry.register(
        "nested_goal_depth_above_handoff_range",
        _nested_goal_depth_above_handoff_range,
    )
    PredicateRegistry.register(
        "nested_goal_depth_gt_1",
        _nested_goal_depth_gt_1,
    )
    PredicateRegistry.register("always", _always)


# ---------------------------------------------------------------------------
# Resolver
# ---------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class AgentSelection:
    """Resolver output: the picked agent + its recipe + extra blocks."""

    agent_def: AgentDefinition
    context_recipe: str
    required_context_blocks: tuple[ContextBlock, ...] = ()
    reason: str | None = None


class RuleBasedAgentResolver:
    """Variants-driven resolver. Frontmatter is the source of truth."""

    def resolve(
        self,
        *,
        base_agent_name: str,
        scope: ContextScope,
        deps: ContextEngineDeps,
    ) -> AgentSelection:
        base = self._load_definition(base_agent_name)

        if not base.variants:
            return AgentSelection(
                agent_def=base,
                context_recipe=self._require_recipe(base),
            )

        ctx = ResolverContext(scope=scope, deps=deps)
        for variant in base.variants:
            predicate = PredicateRegistry.get(variant.when)
            if predicate(ctx):
                return self._select_variant_target(variant)

        return AgentSelection(
            agent_def=base,
            context_recipe=self._require_recipe(base),
        )

    # ---- internals ------------------------------------------------------

    @staticmethod
    def _load_definition(name: str) -> AgentDefinition:
        definition = get_definition(name)
        if definition is None:
            raise AgentDefinitionValidationError(
                f"Agent definition {name!r} is not registered."
            )
        return definition

    def _select_variant_target(self, variant: AgentVariant) -> AgentSelection:
        target = self._load_definition(variant.use)
        if target.variants:
            raise AgentDefinitionValidationError(
                f"Variant target {target.name!r} declares its own variants — "
                "chaining is forbidden."
            )
        return AgentSelection(
            agent_def=target,
            context_recipe=self._require_recipe(target),
            required_context_blocks=tuple(
                _to_context_block(b) for b in variant.required_context_blocks
            ),
            reason=variant.note or None,
        )

    @staticmethod
    def _require_recipe(definition: AgentDefinition) -> str:
        if not definition.context_recipe:
            raise MissingContextRecipeError(
                f"Agent {definition.name!r} has no context_recipe declared in "
                "frontmatter; it cannot be launched via ContextComposer."
            )
        return definition.context_recipe


def _to_context_block(block: AgentSelectionBlock) -> ContextBlock:
    """Convert frontmatter-safe block into a real :class:`ContextBlock`."""
    return ContextBlock(
        kind=block.kind,
        priority=ContextPriority(block.priority),
        text=block.text,
        source_id=block.source_id,
        source_kind=block.source_kind,
        metadata=dict(block.metadata),
    )

"""AgentResolver — frontmatter-driven variant resolution.

The resolver walks the base agent's ``variants:`` list in declared order and
returns the first matching target. Empty variants take a fast path. The
resolver is role-agnostic — every kind of agent (planner, generator,
evaluator, helper) goes through the same code path.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Protocol

from agents.registry import get_definition
from agents.types import AgentDefinition, AgentSelectionBlock
from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import (
    AgentDefinitionValidationError,
    MissingContextRecipeError,
)
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPriority,
)
from task_center.context_engine.predicates import (
    PredicateRegistry,
    ResolverContext,
)
from task_center.context_engine.scope import ContextScope


@dataclass(frozen=True, slots=True)
class AgentSelection:
    """Resolver output: the picked agent + its recipe + extra blocks."""

    agent_def: AgentDefinition
    context_recipe: str
    required_context_blocks: tuple[ContextBlock, ...] = ()
    reason: str | None = None


class AgentResolver(Protocol):
    """Selects an agent definition for a given scope."""

    def resolve(
        self,
        *,
        base_agent_name: str,
        scope: ContextScope,
        deps: ContextEngineDeps,
    ) -> AgentSelection: ...


@dataclass(frozen=True, slots=True)
class RuleBasedAgentResolver:
    """Variants-driven resolver. Frontmatter is the source of truth."""

    predicate_registry: type[PredicateRegistry] = field(default=PredicateRegistry)

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
            predicate = self.predicate_registry.get(variant.when)
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

    def _select_variant_target(self, variant) -> AgentSelection:  # type: ignore[no-untyped-def]
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

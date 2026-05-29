"""Terminal-tool routing for TaskCenter agent launches.

The registered agent profile is stable; this module filters the profile's
terminal tools for a specific launch context. The returned agent definition is
an effective copy, so the registry remains unchanged while prompts and real
tool registration see the same launch-specific terminal set.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from agents import get_definition
from agents import AgentDefinition, AgentKind
from task_center.context_engine.core import (
    AgentDefinitionValidationError,
    ContextEngineDeps,
    MissingContextRecipeError,
)
from task_center.context_engine.scope import ContextScope
from task_center.workflow.ancestry import nested_workflow_depth


@dataclass(frozen=True, slots=True)
class TerminalRoutingContext:
    """Identity + dependency bundle for launch-time terminal routing."""

    scope: ContextScope
    deps: ContextEngineDeps


def _depth(ctx: TerminalRoutingContext) -> int:
    """Return the nested workflow depth for ``ctx``.

    Scopes without a workflow have no caller-attempt ancestry by construction,
    so depth is zero.
    """
    workflow_id = ctx.scope.workflow_id
    if workflow_id is None:
        return 0
    return nested_workflow_depth(
        workflow_id=workflow_id,
        workflow_store=ctx.deps.workflow_store,
        iteration_store=ctx.deps.iteration_store,
        attempt_store=ctx.deps.attempt_store,
        task_store=ctx.deps.task_store,
    )


def _nested_workflow_depth_gt_1(ctx: TerminalRoutingContext) -> bool:
    """True when depth > 1 — caller attempt is itself inside another goal.

    Kept as a named predicate (rather than inlined as ``_depth(ctx) > 1``)
    because router tests in ``test_terminal_tool_router.py`` patch this
    symbol via ``monkeypatch.setattr`` on its full module path to stub the
    depth check without constructing a real goal hierarchy.
    """
    return _depth(ctx) > 1


# ---------------------------------------------------------------------------
# Router
# ---------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class TerminalToolSelection:
    """Router output: effective agent definition + context recipe."""

    agent_def: AgentDefinition
    context_recipe: str
    skill_path: Path | None = None


class TerminalToolRouter:
    """Depth-aware terminal router. Frontmatter remains the source of truth."""

    def resolve(
        self,
        *,
        base_agent_name: str,
        scope: ContextScope,
        deps: ContextEngineDeps,
    ) -> TerminalToolSelection:
        base = self._load_definition(base_agent_name)
        recipe = self._require_recipe(base)
        ctx = TerminalRoutingContext(scope=scope, deps=deps)
        effective = self._effective_definition(base, ctx)
        return TerminalToolSelection(
            agent_def=effective,
            context_recipe=recipe,
            skill_path=effective.skill,
        )

    # ---- internals ---------------------------------------------------------

    @staticmethod
    def _load_definition(name: str) -> AgentDefinition:
        definition = get_definition(name)
        if definition is None:
            raise AgentDefinitionValidationError(f"Agent definition {name!r} is not registered.")
        return definition

    @staticmethod
    def _require_recipe(definition: AgentDefinition) -> str:
        if not definition.context_recipe:
            raise MissingContextRecipeError(
                f"Agent {definition.name!r} has no context_recipe declared in "
                "frontmatter; it cannot be launched via AgentEntryComposer."
            )
        return definition.context_recipe

    def _effective_definition(
        self,
        definition: AgentDefinition,
        ctx: TerminalRoutingContext,
    ) -> AgentDefinition:
        allowed = self._allowed_terminals(definition, ctx)
        if allowed is None:
            return definition
        terminals = [name for name in definition.terminals if name in allowed]
        if terminals == definition.terminals:
            return definition
        return definition.model_copy(update={"terminals": terminals})

    @staticmethod
    def _allowed_terminals(
        definition: AgentDefinition,
        ctx: TerminalRoutingContext,
    ) -> frozenset[str] | None:
        if definition.agent_kind not in {AgentKind.PLANNER, AgentKind.EXECUTOR}:
            return None
        if definition.agent_kind == AgentKind.EXECUTOR and ctx.scope.workflow_id is None:
            return None

        depth_restricted = _nested_workflow_depth_gt_1(ctx)
        if definition.agent_kind == AgentKind.PLANNER:
            if depth_restricted:
                return frozenset({"submit_plan_closes_goal"})
            return frozenset({"submit_plan_closes_goal", "submit_plan_defers_goal"})
        if depth_restricted:
            return frozenset({"submit_execution_success", "submit_execution_blocker"})
        return frozenset(
            {
                "submit_execution_handoff",
                "submit_execution_success",
                "submit_execution_blocker",
            }
        )
